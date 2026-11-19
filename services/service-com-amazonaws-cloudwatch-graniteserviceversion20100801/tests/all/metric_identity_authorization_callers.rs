// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the REST of the identity-based decision in front of this service's
//! metric operations: the operations its catalogue carries beyond the ones the suite beside
//! this one drives, the OTHER kind of credential an account holds an identity for, and the
//! permissions boundary that caps what a document grants.
//!
//! RULE: a call to a CloudWatch operation whose IAM action supports no resource types (the
//! IAM Service Authorization Reference lists each with an empty "Resource types" column, so
//! IAM judges it over every resource) is served only where the identity-based policies in
//! force on the SIGNING PRINCIPAL allow `cloudwatch:<that operation>` over every resource and
//! the permissions boundary in force on that principal allows it too — "a permissions boundary
//! sets the maximum permissions that an identity-based policy can grant" (IAM, permissions
//! boundaries) — whichever operation of that set is asked for and whichever of the account's
//! credentials signed: a session of a role, or an IAM user's own long-term access key, which
//! the refusal then names as the principal. The refusal is HTTP 403 under the code a client
//! branches on for this class, worded as the principal, the action, the resource it was judged
//! against and the rule that withheld the grant.
//!
//! The rows differ along what the rule generalises over: WHICH operation of the catalogue is
//! called (the graph render, and each of the three anomaly-detector operations — the ones the
//! neighbouring suite's table does not reach), WHICH kind of credential signs (an assumed-role
//! session there, an IAM user's long-term key here), what the principal's own documents say
//! (nothing, the action asked for, another action of this service, every action of it, every
//! action plus an explicit deny of this one), and what the BOUNDARY says about the grant those
//! documents gave (every action of the service, only another action, the action plus an
//! explicit deny of it).
//!
//! What the neighbour pins and this one does not repeat:
//! `metric_identity_authorization` drives the publish and the metric reads for an assumed-role
//! session, the account's own published credential, and the fact that a refused publish lands
//! no datapoint. The wordings asserted here are the tree's own
//! (`services/common/src/iam_authorization.rs`), called rather than re-spelled, so one refusal
//! of one class is worded one way across the tree.

use serde_json::{json, Value};
use service_common::iam_authorization::{
    no_identity_policy_allows, no_permissions_boundary_allows, not_authorized_by_identity_policy,
    IDENTITY_POLICY_EXPLICIT_DENY, PERMISSIONS_BOUNDARY_EXPLICIT_DENY,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::metric_identity_authorization::{
    assumed_role, body_of, call_as, emulator, setup_query, statement, xml_element, Principal,
    ACCESS_DENIED, ACCOUNT_KEY, DENIED_STATUS, METRIC, NAMESPACE,
};

/// IAM's own API version, which its `awsQuery` calls name.
const IAM_API_VERSION: &str = "2010-05-08";

/// IAM's signing name, which routes a setup call to it.
const IAM_SIGNING_NAME: &str = "iam";

/// The resource an action that supports no resource types is judged against, as IAM writes it
/// for such a request: every resource.
const EVERY_RESOURCE: &str = "*";

/// How a policy statement spells one of this service's actions: "Actions defined by Amazon
/// CloudWatch" are published under this prefix (IAM Service Authorization Reference), which is
/// not the name the API is signed and reached under.
const ACTION_PREFIX: &str = "cloudwatch";

/// The IAM action `operation` is performed under.
fn action_of(operation: &str) -> String {
    format!("{ACTION_PREFIX}:{operation}")
}

/// One identity-based or boundary document carrying one statement per entry of `entries`, each
/// over every resource.
///
/// A permissions boundary is attached as ONE managed policy, so a boundary that both grants and
/// denies has to state both in a single document — which is what this builds, where the
/// neighbour's one-statement [`statement`] cannot.
fn document(entries: &[(&str, &str)]) -> String {
    let listed: Vec<Value> = entries
        .iter()
        .map(|(effect, action)| {
            json!({"Effect": effect, "Action": action, "Resource": EVERY_RESOURCE})
        })
        .collect();
    json!({"Version": "2012-10-17", "Statement": listed}).to_string()
}

/// The whole sentence a refusal of `action` carries for `principal`, built by the tree's own
/// wording of this refusal rather than re-spelled here.
fn refusal_message(principal: &str, action: &str, reason: &str) -> String {
    not_authorized_by_identity_policy(principal, action, EVERY_RESOURCE, reason)
}

/// The request body of one catalogue operation under test.
///
/// Each is the smallest request the model admits: `MetricWidget` is "A JSON string that defines
/// the bitmap graph to be retrieved" (`GetMetricWidgetImageInput$MetricWidget`), and the three
/// anomaly-detector operations address a model by the `SingleMetricAnomalyDetector` structure
/// the model declares for them.
fn catalogue_body(operation: &str) -> Value {
    match operation {
        "GetMetricWidgetImage" => json!({
            "MetricWidget": json!({"metrics": [[NAMESPACE, METRIC, "Service", "api", {}]]})
                .to_string(),
        }),
        "PutAnomalyDetector" | "DeleteAnomalyDetector" => json!({
            "SingleMetricAnomalyDetector": {
                "Namespace": NAMESPACE,
                "MetricName": METRIC,
                "Stat": "Average",
            },
        }),
        "DescribeAnomalyDetectors" => json!({"Namespace": NAMESPACE}),
        other => panic!("no request body is stated for {other}"),
    }
}

/// Create the anomaly detector a delete addresses, with the account's own credential — so the
/// row that deletes it is decided on its own role's documents and not on whether the model
/// happened to exist.
async fn plant_detector(router: &MultiServiceRouter, case: &str) {
    let (status, answered) = call_as(
        router,
        ACCOUNT_KEY,
        "PutAnomalyDetector",
        &catalogue_body("PutAnomalyDetector"),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: planting the detector must succeed, got {status} {answered}"
    );
}

/// One row of the catalogue table: the single action the calling role is granted, the operation
/// it then asks for, and whether IAM lets it.
struct CatalogueRow {
    case: &'static str,
    /// The one action the role's document allows, or `None` where the row grants the very action
    /// the operation is performed under.
    granted: Option<&'static str>,
    operation: &'static str,
    allowed: bool,
}

/// The operations of this service's gate whose decision the neighbouring suite's table does not
/// reach, each asked for twice: by a role granted exactly its action, and by a role granted
/// another action of this service instead.
fn catalogue_rows() -> Vec<CatalogueRow> {
    let mut rows = Vec::new();
    for operation in [
        "GetMetricWidgetImage",
        "PutAnomalyDetector",
        "DescribeAnomalyDetectors",
        "DeleteAnomalyDetector",
    ] {
        rows.push(CatalogueRow {
            case: "a grant of the action asked for",
            granted: None,
            operation,
            allowed: true,
        });
        rows.push(CatalogueRow {
            case: "a grant of another action of this service",
            granted: Some("cloudwatch:ListMetrics"),
            operation,
            allowed: false,
        });
    }
    rows
}

/// RULE: every operation whose IAM action supports no resource types is decided the same way —
/// served where the caller's documents allow that operation's own action over every resource,
/// refused 403 `AccessDenied` where they allow only another of this service's actions.
#[tokio::test]
async fn every_operation_of_the_catalogue_is_decided_against_its_own_action() {
    let router = emulator();
    for (index, row) in catalogue_rows().into_iter().enumerate() {
        let CatalogueRow {
            case,
            granted,
            operation,
            allowed,
        } = row;
        let action = action_of(operation);
        // A row that names no other action grants the one it asks for.
        let held = granted.map_or_else(|| action.clone(), ToString::to_string);
        let role = format!("cw-catalogue-{index}");
        let principal =
            assumed_role(&router, &role, &[statement("Allow", &held, EVERY_RESOURCE)]).await;
        if allowed && operation == "DeleteAnomalyDetector" {
            plant_detector(&router, case).await;
        }
        let (status, answered) = call_as(
            &router,
            &principal.access_key_id,
            operation,
            &catalogue_body(operation),
        )
        .await;
        if allowed {
            assert_eq!(
                status, 200,
                "case {case:?}: expected {operation} to be served, got {status} {answered}"
            );
            continue;
        }
        assert_eq!(
            status, DENIED_STATUS,
            "case {case:?}: expected {operation} to be refused {DENIED_STATUS}, got {status} \
             {answered}"
        );
        assert_eq!(
            answered["__type"],
            json!(ACCESS_DENIED),
            "case {case:?}: expected the {ACCESS_DENIED} class for {operation}, got {answered}"
        );
        let expected = refusal_message(&principal.arn, &action, &no_identity_policy_allows(&action));
        assert_eq!(
            answered["message"],
            json!(expected),
            "case {case:?}: expected {operation} to be refused with {expected:?}, got {answered}"
        );
    }
}

/// Create an IAM user carrying one inline policy per document in `policies`, issue it a
/// long-term access key, and answer that key with the ARN IAM holds the user by.
async fn iam_user(
    router: &MultiServiceRouter,
    user_name: &str,
    policies: &[String],
) -> Principal {
    let created = setup_query(
        router,
        IAM_SIGNING_NAME,
        IAM_API_VERSION,
        "CreateUser",
        &[("UserName", user_name.to_string())],
    )
    .await;
    let arn = xml_element(&created, "Arn")
        .unwrap_or_else(|| panic!("CreateUser answers the user's Arn, got {created}"));
    // SEVERAL inline policies, so a decision taken over the union of the documents in force is
    // distinguishable from one taken inside a single document.
    for (index, policy) in policies.iter().enumerate() {
        setup_query(
            router,
            IAM_SIGNING_NAME,
            IAM_API_VERSION,
            "PutUserPolicy",
            &[
                ("UserName", user_name.to_string()),
                ("PolicyName", format!("inline-{index}")),
                ("PolicyDocument", policy.clone()),
            ],
        )
        .await;
    }
    let issued = setup_query(
        router,
        IAM_SIGNING_NAME,
        IAM_API_VERSION,
        "CreateAccessKey",
        &[("UserName", user_name.to_string())],
    )
    .await;
    Principal {
        access_key_id: xml_element(&issued, "AccessKeyId")
            .unwrap_or_else(|| panic!("CreateAccessKey answers an AccessKeyId, got {issued}")),
        arn,
    }
}

/// One row of the long-term-key table: the user a call signs as, the documents in force on it,
/// the operation it asks for, and what must come back.
struct UserRow {
    case: &'static str,
    user: &'static str,
    /// The identity-based documents in force on that user, one inline policy each.
    policies: Vec<String>,
    operation: &'static str,
    /// The reason a refusal must carry, or `None` where the call must be served.
    reason: Option<String>,
}

/// The rows of the long-term-key table.
fn user_rows() -> Vec<UserRow> {
    let publish = no_identity_policy_allows(&action_of("PutMetricData"));
    vec![
        UserRow {
            case: "no policy at all",
            user: "cw-user-no-policy",
            policies: Vec::new(),
            operation: "PutMetricData",
            reason: Some(publish.clone()),
        },
        UserRow {
            case: "a grant of the action asked for",
            user: "cw-user-grant-put",
            policies: vec![statement(
                "Allow",
                "cloudwatch:PutMetricData",
                EVERY_RESOURCE,
            )],
            operation: "PutMetricData",
            reason: None,
        },
        UserRow {
            case: "a grant of another action of this service",
            user: "cw-user-grant-other",
            policies: vec![statement("Allow", "cloudwatch:ListMetrics", EVERY_RESOURCE)],
            operation: "PutMetricData",
            reason: Some(publish),
        },
        UserRow {
            case: "a grant of every action of this service and an explicit deny of this one",
            user: "cw-user-denied",
            policies: vec![
                statement("Allow", "cloudwatch:*", EVERY_RESOURCE),
                statement("Deny", "cloudwatch:PutMetricData", EVERY_RESOURCE),
            ],
            operation: "PutMetricData",
            reason: Some(IDENTITY_POLICY_EXPLICIT_DENY.to_string()),
        },
        UserRow {
            case: "a grant of the read asked for",
            user: "cw-user-grant-read",
            policies: vec![statement(
                "Allow",
                "cloudwatch:GetMetricStatistics",
                EVERY_RESOURCE,
            )],
            operation: "GetMetricStatistics",
            reason: None,
        },
    ]
}

/// RULE: the principal a request speaks for is the identity the access key id it presents
/// belongs to, so an IAM USER's own long-term key is judged on that user's documents and a
/// refusal names the USER — the same decision an assumed-role session gets, on the other kind
/// of credential the account holds an identity for.
#[tokio::test]
async fn a_long_term_access_key_is_judged_on_its_users_own_policies() {
    let router = emulator();
    for row in user_rows() {
        let UserRow {
            case,
            user,
            policies,
            operation,
            reason,
        } = row;
        let principal = iam_user(&router, user, &policies).await;
        // The principal of a long-term key is the USER, not a session: that is what a refusal
        // below has to blame, so the ARN IAM answered is checked to be the user's own.
        assert!(
            principal.arn.ends_with(&format!(":user/{user}")),
            "case {case:?}: expected IAM to hold the user by its own ARN, got {:?}",
            principal.arn
        );
        let (status, answered) = call_as(
            &router,
            &principal.access_key_id,
            operation,
            &body_of(operation),
        )
        .await;
        let Some(reason) = reason else {
            assert_eq!(
                status, 200,
                "case {case:?}: expected {operation} to be served, got {status} {answered}"
            );
            continue;
        };
        assert_eq!(
            status, DENIED_STATUS,
            "case {case:?}: expected {operation} to be refused {DENIED_STATUS}, got {status} \
             {answered}"
        );
        assert_eq!(
            answered["__type"],
            json!(ACCESS_DENIED),
            "case {case:?}: expected the {ACCESS_DENIED} class, got {answered}"
        );
        let expected = refusal_message(&principal.arn, &action_of(operation), &reason);
        assert_eq!(
            answered["message"],
            json!(expected),
            "case {case:?}: expected the refusal {expected:?}, got {answered}"
        );
    }
}

/// Attach `boundary` to `role` as its permissions boundary, through the managed policy a
/// boundary is attached as ("the ARN of the managed policy that is used to set the permissions
/// boundary", `PutRolePermissionsBoundary`).
async fn bounded_by(router: &MultiServiceRouter, role: &str, boundary: String) {
    let created = setup_query(
        router,
        IAM_SIGNING_NAME,
        IAM_API_VERSION,
        "CreatePolicy",
        &[
            ("PolicyName", format!("{role}-boundary")),
            ("PolicyDocument", boundary),
        ],
    )
    .await;
    let policy_arn = xml_element(&created, "Arn")
        .unwrap_or_else(|| panic!("CreatePolicy answers the policy's Arn, got {created}"));
    setup_query(
        router,
        IAM_SIGNING_NAME,
        IAM_API_VERSION,
        "PutRolePermissionsBoundary",
        &[
            ("RoleName", role.to_string()),
            ("PermissionsBoundary", policy_arn),
        ],
    )
    .await;
}

/// One row of the boundary table: the role a call signs as a session of, what its BOUNDARY
/// says, and what must come back. Every row's own documents allow every action of this service,
/// so what differs between them is the boundary alone.
struct BoundaryRow {
    case: &'static str,
    role: &'static str,
    /// The statements the boundary document carries, each over every resource.
    boundary: Vec<(&'static str, &'static str)>,
    /// The reason a refusal must carry, or `None` where the call must be served.
    reason: Option<String>,
}

/// The rows of the boundary table.
fn boundary_rows() -> Vec<BoundaryRow> {
    vec![
        BoundaryRow {
            case: "a boundary allowing every action of this service",
            role: "cw-boundary-all",
            boundary: vec![("Allow", "cloudwatch:*")],
            reason: None,
        },
        BoundaryRow {
            case: "a boundary allowing only another action of this service",
            role: "cw-boundary-other",
            boundary: vec![("Allow", "cloudwatch:ListMetrics")],
            reason: Some(no_permissions_boundary_allows(&action_of("PutMetricData"))),
        },
        BoundaryRow {
            case: "a boundary allowing every action and explicitly denying this one",
            role: "cw-boundary-denied",
            boundary: vec![
                ("Allow", "cloudwatch:*"),
                ("Deny", "cloudwatch:PutMetricData"),
            ],
            reason: Some(PERMISSIONS_BOUNDARY_EXPLICIT_DENY.to_string()),
        },
    ]
}

/// RULE: a permissions boundary sets the MAXIMUM permissions an identity-based policy can
/// grant, so a grant the boundary does not also allow is not a permission the principal has —
/// and a boundary that explicitly denies the action refuses it whatever the documents grant.
#[tokio::test]
async fn a_permissions_boundary_caps_the_grant_the_documents_gave() {
    let router = emulator();
    for row in boundary_rows() {
        let BoundaryRow {
            case,
            role,
            boundary,
            reason,
        } = row;
        let principal = assumed_role(
            &router,
            role,
            &[statement("Allow", "cloudwatch:*", EVERY_RESOURCE)],
        )
        .await;
        bounded_by(&router, role, document(&boundary)).await;
        let (status, answered) = call_as(
            &router,
            &principal.access_key_id,
            "PutMetricData",
            &body_of("PutMetricData"),
        )
        .await;
        let Some(reason) = reason else {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the publish to be served, got {status} {answered}"
            );
            continue;
        };
        assert_eq!(
            status, DENIED_STATUS,
            "case {case:?}: expected the publish to be refused {DENIED_STATUS}, got {status} \
             {answered}"
        );
        let expected = refusal_message(&principal.arn, &action_of("PutMetricData"), &reason);
        assert_eq!(
            answered["message"],
            json!(expected),
            "case {case:?}: expected the refusal {expected:?}, got {answered}"
        );
    }
}
