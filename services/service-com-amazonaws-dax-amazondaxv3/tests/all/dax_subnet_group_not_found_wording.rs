// SPDX-License-Identifier: CC0-1.0

//! The wording of `SubnetGroupNotFoundFault`, per KIND of path that raises it.
//!
//! One fault shape answers three different kinds of request on this service, and each
//! kind has its own message template: a request ADDRESSED to the one subnet group it
//! names (a modification of that group) is answered "Subnet Group <name> does not
//! exist."; a request that names the group as one element of a requested LIST is answered
//! "Subnet group <name> not found."; and a request that merely REFERENCES a group from
//! another resource (a cluster placed in it) is answered "Subnet Group not found.",
//! naming no identifier because the request already carries the only group it could mean.
//!
//! Each template's variable part is the group's identity as this service holds it —
//! `CreateSubnetGroup$SubnetGroupName` documents that the name "is stored as a lowercase
//! string" — never a value written out here.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::dax_account_network::{an_account_subnet, call, emulator, error_code, error_message};

/// The status this service's `awsJson1_1` wire puts on every modelled client fault.
const CLIENT_FAULT_STATUS: u16 = 400;

/// A role this service accepts for a cluster, and a node type it offers: a cluster create
/// is one of the paths that REFERENCES a subnet group.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/DAXRole";

/// A node type this service offers.
const NODE_TYPE: &str = "dax.r4.large";

/// A `CreateCluster` request placed in `subnet_group`.
fn create_cluster_in(name: &str, subnet_group: &str) -> Value {
    json!({
        "ClusterName": name,
        "NodeType": NODE_TYPE,
        "ReplicationFactor": 1,
        "IamRoleArn": ROLE_ARN,
        "SubnetGroupName": subnet_group,
    })
}

/// Call `operation` expecting `SubnetGroupNotFoundFault`, and return the message it
/// carried.
async fn refusal_message(router: &MultiServiceRouter, operation: &str, request: Value) -> String {
    let (status, body) = call(router, operation, request).await;
    assert_eq!(
        status, CLIENT_FAULT_STATUS,
        "case {operation}: expected {CLIENT_FAULT_STATUS}, got {status} {body:?}"
    );
    assert_eq!(
        error_code(&body),
        "SubnetGroupNotFoundFault",
        "case {operation}: expected SubnetGroupNotFoundFault, got {body:?}"
    );
    error_message(&body)
}

// ---------------------------------------------------------------------------
// Rule: a request ADDRESSED to the one subnet group it names and resolving to nothing is
// refused with `SubnetGroupNotFoundFault` worded "Subnet Group <name> does not exist.",
// whose only variable part is the group identity the request asked for, lower-cased the
// way this service stores subnet group names.
// Source: the recorded DAX answer on `UpdateSubnetGroup` against an absent group, and
// `CreateSubnetGroup$SubnetGroupName` ("This value is stored as a lowercase string").
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_modification_addressed_to_an_absent_group_says_it_does_not_exist() {
    let router = emulator();
    let subnet = an_account_subnet(&router).await;

    // Rows differ along what the template generalises over: the length of the name, its
    // casing, its hyphens and digits, a probe-shaped name, and whether the modification
    // also carries a description, a membership, both or neither.
    let names = [
        "ghost",
        "g",
        "MixedCaseGhost",
        "ghost-with-hyphens",
        "sim2real-probe-dax-missing-sng-23bbaef0",
        "group2000",
    ];
    for name in names {
        let shapes: [(&str, Value); 4] = [
            ("neither", json!({ "SubnetGroupName": name })),
            (
                "description",
                json!({ "SubnetGroupName": name, "Description": "never applied" }),
            ),
            (
                "membership",
                json!({ "SubnetGroupName": name, "SubnetIds": [subnet.clone()] }),
            ),
            (
                "both",
                json!({
                    "SubnetGroupName": name,
                    "Description": "never applied",
                    "SubnetIds": [subnet.clone()],
                }),
            ),
        ];
        let expected = format!("Subnet Group {} does not exist.", name.to_lowercase());
        for (shape, request) in shapes {
            let answered = refusal_message(&router, "UpdateSubnetGroup", request).await;
            assert_eq!(
                answered, expected,
                "case {name:?} carrying {shape}: expected {expected:?}, got {answered:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Rule: each KIND of path raising `SubnetGroupNotFoundFault` keeps its own template — the
// group ADDRESSED by a modification is "Subnet Group <name> does not exist.", one element
// of a requested LIST is "Subnet group <name> not found.", and a group only REFERENCED by
// another resource is "Subnet Group not found." — so the same absent name is worded by
// the path that asked for it, and a group that IS stored is not refused at all.
// Source: the recorded DAX answers on `UpdateSubnetGroup`, on `DescribeSubnetGroups`
// taking `SubnetGroupNames`, and on `CreateCluster` naming a `SubnetGroupName`.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn each_kind_of_subnet_group_lookup_keeps_its_own_template() {
    let router = emulator();
    let subnet = an_account_subnet(&router).await;

    // One group that DOES exist, so the templates below are the answer to a name that
    // resolves to nothing rather than to every name.
    let (status, body) = call(
        &router,
        "CreateSubnetGroup",
        json!({
            "SubnetGroupName": "stored",
            "Description": "as created",
            "SubnetIds": [subnet.clone()],
        }),
    )
    .await;
    assert_eq!(status, 200, "CreateSubnetGroup stored: got {status} {body:?}");

    for (asked, identity) in [("absent-sg", "absent-sg"), ("Absent-SG", "absent-sg")] {
        let addressed = refusal_message(
            &router,
            "UpdateSubnetGroup",
            json!({ "SubnetGroupName": asked, "Description": "never applied" }),
        )
        .await;
        assert_eq!(
            addressed,
            format!("Subnet Group {identity} does not exist."),
            "case {asked:?} addressed: got {addressed:?}"
        );

        let listed = refusal_message(
            &router,
            "DescribeSubnetGroups",
            json!({ "SubnetGroupNames": [asked] }),
        )
        .await;
        assert_eq!(
            listed,
            format!("Subnet group {identity} not found."),
            "case {asked:?} listed: got {listed:?}"
        );

        // A list mixing the stored group with the absent one is still refused naming the
        // ABSENT one, which is why that template carries a name at all.
        let mixed = refusal_message(
            &router,
            "DescribeSubnetGroups",
            json!({ "SubnetGroupNames": ["stored", asked] }),
        )
        .await;
        assert_eq!(
            mixed,
            format!("Subnet group {identity} not found."),
            "case {asked:?} in a mixed list: got {mixed:?}"
        );

        let referenced = refusal_message(
            &router,
            "CreateCluster",
            create_cluster_in(&format!("needs-{identity}"), asked),
        )
        .await;
        assert_eq!(
            referenced, "Subnet Group not found.",
            "case {asked:?} referenced: this template names no identifier, got {referenced:?}"
        );
    }

    // The group that IS stored is modified, not refused: the wording above belongs to the
    // lookup failure alone, and the stored group reads back with what the modify applied.
    let (status, body) = call(
        &router,
        "UpdateSubnetGroup",
        json!({ "SubnetGroupName": "Stored", "Description": "modified" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "UpdateSubnetGroup stored must succeed, got {status} {body:?}"
    );
    let (status, read) = call(
        &router,
        "DescribeSubnetGroups",
        json!({ "SubnetGroupNames": ["stored"] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeSubnetGroups stored must report the group, got {status} {read:?}"
    );
    assert_eq!(
        read["SubnetGroups"][0]["Description"],
        json!("modified"),
        "the stored group must carry what the modify applied, got {read:?}"
    );
}
