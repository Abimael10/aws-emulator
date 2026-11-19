// SPDX-License-Identifier: CC0-1.0

//! The IAM role a registered hybrid node is MOVED to, driven end to end over this service
//! and IAM on one shared engine.
//!
//! RULE: a hybrid node is managed UNDER the role it is assigned — "This IAM role must
//! provide AssumeRole permissions for the Amazon Web Services Systems Manager service
//! principal ssm.amazonaws.com" (`smithy_query(query="doc",
//! shape="UpdateManagedInstanceRoleRequest", member="IamRole")`) — so `UpdateManagedInstanceRole`
//! resolves the role it carries against the account's IAM records at the moment of the call
//! and moves the node only to a role whose trust policy grants that assume: the moved node
//! reports the role every node read of this service answers with, a refused update leaves the
//! role the last accepted one wrote, and the node id is resolved FIRST, so an id the Region
//! holds no node under is refused `InvalidInstanceId` whatever role the request names.
//!
//! The two refusals differ in kind, which is why each is asserted on its own: the node id is
//! the operation's OWN modelled client error (`InvalidInstanceId`, carrying no message), while
//! a role this service could never act as is refused as a bad request — the account's
//! `ValidationException` carrying the one unassumable-role sentence this service words for
//! every member that hands it a role ([`crate::hybrid_activation_role::expected_refusal`],
//! which is the sibling suite's reading of what the create of an activation answered).
//!
//! The rows span what the rule generalises over: how the trust policy states the grant (the
//! principal alone, among several), what it states instead (another service's principal, no
//! statement at all), a name IAM holds no role under, where IAM holds the role (at the root or
//! under a path), the length of the role name up to the bound the member admits, and the form
//! of the node id the update names (an `mi-` id and an `eks_c:` one).
//!
//! Every role is created through IAM's OWN operations and every update and read through this
//! service's own, over the shared front-door harness in `all/parameter_store.rs`. The ONE
//! thing seeded through the engine is the registered node itself, because no call of this
//! model registers a hybrid node ([`crate::managed_node_lifecycle::register`]).

use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::RESOURCE_TYPE_MANAGED_INSTANCE;
use smithy_server_runtime::MultiServiceRouter;

use crate::hybrid_activation_role::{
    create_role, expected_refusal, iam, iam_role_value, router_with_iam_and_engine, statement,
    trust, trusted_by_ssm, ASSUME_ROLE, SSM_PRINCIPAL,
};
use crate::managed_node_lifecycle::register;
use crate::parameter_store::{call, error_code, error_message, REGION};

/// A hybrid node id: `mi-` and 17 hexadecimal characters, one form `ManagedInstanceId`
/// admits (`^(^mi-[0-9a-f]{17}$)|(^eks_c:[0-9A-Za-z][A-Za-z0-9\-_]{0,99}_\w{17}$)$`).
const HYBRID_NODE: &str = "mi-0a1b2c3d4e5f60718";

/// A node id of the OTHER form that shape admits: a cluster-scoped `eks_c:` id.
const EKS_NODE: &str = "eks_c:managed-cluster_abcdefghij1234567";

/// A hybrid node id nothing registered.
const UNREGISTERED_NODE: &str = "mi-99999999999999999";

/// A second Region, so a case can name a node this endpoint does not hold.
const OTHER_REGION: &str = "us-east-1";

/// The role every node is moved to before the row's own update runs, so a REFUSED update is
/// observable as the role it did not replace. Trusted by this service in every row.
const BASELINE_ROLE: &str = "ssm-node-baseline-role";

/// The longest role name the member admits: `UpdateManagedInstanceRoleRequest$IamRole`
/// targets `IamRole`, a string of length 0..64 (`smithy_query(query="shape",
/// shape="IamRole")`), which is IAM's own `RoleName` bound too.
const MAX_IAM_ROLE_LENGTH: usize = 64;

/// A role name exactly [`MAX_IAM_ROLE_LENGTH`] characters long.
const LONGEST_ROLE: &str = "ssm-node-role-with-the-longest-name-the-member-admits-abcdefghij";

/// What an update naming one role is answered with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Verdict {
    /// IAM holds a role this service may assume: the node is moved to it.
    Moved,
    /// IAM answered that the role cannot be assumed: the update is refused and the node
    /// keeps the role it had.
    Refused,
}

/// One row of the table.
struct Row {
    case: &'static str,
    /// The node the update names.
    node: &'static str,
    /// The IAM path the role is created under.
    path: &'static str,
    /// The role name, unique within the row's own IAM store.
    role_name: &'static str,
    /// The role's `AssumeRolePolicyDocument`, or `None` for a row that creates no role at
    /// all and names one IAM holds nothing under.
    trust: Option<Value>,
    verdict: Verdict,
}

/// A trust policy whose one statement grants `action` to the service principals
/// `principals` names, through the sibling suite's trust-policy helpers.
fn grants(principals: Value, action: Value) -> Value {
    trust(json!([statement("Allow", principals, action)]))
}

/// The table.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a role trusting this service, at the root",
            node: HYBRID_NODE,
            path: "/",
            role_name: "ssm-node-plain",
            trust: Some(trusted_by_ssm()),
            verdict: Verdict::Moved,
        },
        Row {
            case: "a role IAM holds under a path",
            node: HYBRID_NODE,
            path: "/service-role/",
            role_name: "ssm-node-scoped",
            trust: Some(trusted_by_ssm()),
            verdict: Verdict::Moved,
        },
        Row {
            case: "a role trusting this service among several",
            node: HYBRID_NODE,
            path: "/",
            role_name: "ssm-node-among-several",
            trust: Some(grants(
                json!(["ec2.amazonaws.com", SSM_PRINCIPAL]),
                json!(["sts:TagSession", ASSUME_ROLE]),
            )),
            verdict: Verdict::Moved,
        },
        Row {
            case: "the longest role name the member admits",
            node: HYBRID_NODE,
            path: "/",
            role_name: LONGEST_ROLE,
            trust: Some(trusted_by_ssm()),
            verdict: Verdict::Moved,
        },
        Row {
            case: "a cluster-scoped node moved to a trusted role",
            node: EKS_NODE,
            path: "/",
            role_name: "ssm-cluster-node-role",
            trust: Some(trusted_by_ssm()),
            verdict: Verdict::Moved,
        },
        Row {
            case: "a role trusting another service only",
            node: HYBRID_NODE,
            path: "/",
            role_name: "ssm-node-other-service",
            trust: Some(grants(json!("ec2.amazonaws.com"), json!(ASSUME_ROLE))),
            verdict: Verdict::Refused,
        },
        Row {
            case: "a role granting this service another action, not the assume",
            node: HYBRID_NODE,
            path: "/",
            role_name: "ssm-node-other-action",
            trust: Some(grants(json!(SSM_PRINCIPAL), json!("sts:TagSession"))),
            verdict: Verdict::Refused,
        },
        Row {
            case: "a role whose trust policy grants nothing at all",
            node: HYBRID_NODE,
            path: "/",
            role_name: "ssm-node-no-statement",
            trust: Some(trust(json!([]))),
            verdict: Verdict::Refused,
        },
        Row {
            case: "a name IAM holds no role under",
            node: EKS_NODE,
            path: "/",
            role_name: "ssm-node-nobody-created",
            trust: None,
            verdict: Verdict::Refused,
        },
    ]
}

/// This service and IAM over one engine, with both node forms registered in [`REGION`].
fn emulator() -> MultiServiceRouter {
    let (router, engine) = router_with_iam_and_engine();
    for node_id in [HYBRID_NODE, EKS_NODE] {
        register(
            &engine,
            REGION,
            node_id,
            RESOURCE_TYPE_MANAGED_INSTANCE,
            "Linux",
        );
    }
    router
}

/// One `UpdateManagedInstanceRole` call, answering `(status, body)`.
async fn update_role(
    router: &MultiServiceRouter,
    node_id: &str,
    iam_role: &str,
) -> (u16, Value) {
    call(
        router,
        "UpdateManagedInstanceRole",
        &json!({ "InstanceId": node_id, "IamRole": iam_role }),
    )
    .await
}

/// The nodes `operation` reports for [`REGION`], read under `member`.
async fn reported_nodes(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    member: &str,
) -> Vec<Value> {
    let (status, body) = call(router, operation, &json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must be served, got {status} with {body}"
    );
    body[member]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: {operation} reports {member}, got {body}"))
        .clone()
}

/// Assert that every node read of this service reports `expected` as `node_id`'s `IamRole`.
///
/// Both reads project the SAME stored node, so a role one reports and the other does not
/// would be a role only one of them computed.
async fn assert_reported_role(
    router: &MultiServiceRouter,
    case: &str,
    node_id: &str,
    expected: Option<&str>,
) {
    for (operation, member) in [
        ("DescribeInstanceInformation", "InstanceInformationList"),
        ("DescribeInstanceProperties", "InstanceProperties"),
    ] {
        let nodes = reported_nodes(router, case, operation, member).await;
        let node = nodes
            .iter()
            .find(|node| node["InstanceId"] == json!(node_id))
            .unwrap_or_else(|| {
                panic!("case {case:?}: {operation} must report the node {node_id}, got {nodes:?}")
            });
        assert_eq!(
            node["IamRole"].as_str(),
            expected,
            "case {case:?}: {operation} must report {expected:?} as {node_id}'s IamRole, got \
             {node}"
        );
    }
}

/// Move `node_id` to [`BASELINE_ROLE`], which the row's own IAM store holds and trusts, so
/// the role a refused update must leave in place is one an ACCEPTED update wrote.
async fn move_to_baseline(router: &MultiServiceRouter, case: &str, node_id: &str) {
    create_role(router, case, "/", BASELINE_ROLE, &trusted_by_ssm()).await;
    let (status, body) = update_role(router, node_id, BASELINE_ROLE).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the baseline update must be served, got {status} with {body}"
    );
    assert_reported_role(router, case, node_id, Some(BASELINE_ROLE)).await;
}

/// The node of the two forms that is NOT the one a row names.
fn other_node(node_id: &str) -> &'static str {
    if node_id == HYBRID_NODE {
        EKS_NODE
    } else {
        HYBRID_NODE
    }
}

/// RULE: the update resolves the `IamRole` against the account's IAM records and moves the
/// node only to a role whose trust policy lets this service assume it — and a refused update
/// writes nothing, so the node keeps the role the last accepted update wrote.
#[tokio::test]
async fn a_node_is_moved_only_to_a_role_this_service_may_assume() {
    assert_eq!(
        LONGEST_ROLE.len(),
        MAX_IAM_ROLE_LENGTH,
        "the longest-name row must sit exactly at the modelled bound"
    );
    for row in rows() {
        let case = row.case;
        // A router per row, so each row's IAM store holds only its own roles.
        let router = emulator();
        move_to_baseline(&router, case, row.node).await;
        if let Some(document) = row.trust.as_ref() {
            create_role(&router, case, row.path, row.role_name, document).await;
        }
        let iam_role = iam_role_value(row.path, row.role_name);

        let (status, body) = update_role(&router, row.node, &iam_role).await;

        match row.verdict {
            Verdict::Moved => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: the update must be served, got {status} with {body}"
                );
                assert_reported_role(&router, case, row.node, Some(iam_role.as_str())).await;
            }
            Verdict::Refused => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: the update must be refused, got {status} with {body}"
                );
                assert_eq!(
                    error_code(&body),
                    "ValidationException",
                    "case {case:?}: expected a ValidationException, got {body}"
                );
                let expected = expected_refusal(&iam_role);
                assert_eq!(
                    error_message(&body),
                    expected,
                    "case {case:?}: expected the refusal {expected:?}, got {:?}",
                    error_message(&body)
                );
                // Nothing was written: the role the baseline update wrote is still the one
                // every read reports.
                assert_reported_role(&router, case, row.node, Some(BASELINE_ROLE)).await;
            }
        }

        // ONE node is moved: the other node this Region holds was never assigned a role, so
        // it reports none whatever the row's update did.
        assert_reported_role(&router, case, other_node(row.node), None).await;
    }
}

/// RULE: the node id is resolved BEFORE the role, so an id this Region holds no node under
/// names nothing to move — `InvalidInstanceId` carrying no message, whatever role the request
/// names and whether or not IAM holds it — and nothing is written.
#[tokio::test]
async fn an_id_that_names_no_node_is_refused_before_the_role_is_resolved() {
    // (case, the node id the request names, the role it names, whether IAM holds that role)
    let rows: [(&str, &str, &str, bool); 4] = [
        (
            "an id nothing registered, naming a role this service may assume",
            UNREGISTERED_NODE,
            "ssm-node-held-and-trusted",
            true,
        ),
        (
            "an id nothing registered, naming a role IAM holds nothing under",
            UNREGISTERED_NODE,
            "ssm-node-never-created",
            false,
        ),
        (
            "a cluster-scoped id nothing registered",
            "eks_c:another-cluster_zyxwvutsr98765432",
            "ssm-node-held-and-trusted",
            true,
        ),
        (
            "an id this endpoint's Region holds no node under",
            HYBRID_NODE,
            "ssm-node-held-and-trusted",
            true,
        ),
    ];

    for (case, node_id, iam_role, role_is_held) in rows {
        let (router, engine) = router_with_iam_and_engine();
        // The node of that id IS registered — in another Region, which this request is not
        // signed for, so the id names nothing here.
        register(
            &engine,
            OTHER_REGION,
            HYBRID_NODE,
            RESOURCE_TYPE_MANAGED_INSTANCE,
            "Linux",
        );
        register(
            &engine,
            REGION,
            EKS_NODE,
            RESOURCE_TYPE_MANAGED_INSTANCE,
            "Linux",
        );
        if role_is_held {
            create_role(&router, case, "/", iam_role, &trusted_by_ssm()).await;
        }

        let (status, body) = update_role(&router, node_id, iam_role).await;
        assert_eq!(
            status, 400,
            "case {case:?}: the update must be refused, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidInstanceId",
            "case {case:?}: expected InvalidInstanceId, got {body}"
        );
        assert_eq!(
            error_message(&body),
            "",
            "case {case:?}: this refusal carries no message, got {body}"
        );
        // The node this Region does hold was not touched by the refusal.
        assert_reported_role(&router, case, EKS_NODE, None).await;
    }
}

/// RULE, grounded: the role is resolved per CALL against IAM's records, so replacing a role's
/// trust policy through IAM's own `UpdateAssumeRolePolicy` changes what the next update is
/// answered — and the move the accepted update made is what every later read reports.
#[tokio::test]
async fn the_trust_policy_in_force_at_the_update_decides_it() {
    let case = "a role whose trust policy changes between updates";
    let router = emulator();
    let role_name = "ssm-node-retrusted";
    move_to_baseline(&router, case, HYBRID_NODE).await;
    // Created trusting another service: the first update is refused.
    create_role(
        &router,
        case,
        "/",
        role_name,
        &grants(json!("ec2.amazonaws.com"), json!(ASSUME_ROLE)),
    )
    .await;
    let (status, body) = update_role(&router, HYBRID_NODE, role_name).await;
    assert_eq!(
        status, 400,
        "case {case:?}: the first update must be refused, got {status} with {body}"
    );
    assert_reported_role(&router, case, HYBRID_NODE, Some(BASELINE_ROLE)).await;

    // Re-trusted through IAM itself: the next update is served, and the node reports it.
    iam(
        &router,
        case,
        "UpdateAssumeRolePolicy",
        &[
            ("RoleName", role_name.to_string()),
            ("PolicyDocument", trusted_by_ssm().to_string()),
        ],
    )
    .await;
    let (status, body) = update_role(&router, HYBRID_NODE, role_name).await;
    assert_eq!(
        status, 200,
        "case {case:?}: after the trust policy admits this service, the update must be \
         served, got {status} with {body}"
    );
    assert_reported_role(&router, case, HYBRID_NODE, Some(role_name)).await;
}
