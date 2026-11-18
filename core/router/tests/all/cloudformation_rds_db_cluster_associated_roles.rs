// SPDX-License-Identifier: CC0-1.0

//! The IAM role associations a CloudFormation-deployed `AWS::RDS::DBCluster` declares — at the
//! create, and at a stack UPDATE that adds one.
//!
//! The rule under test: **a declared property is carried to the owning service by the call
//! that service makes the thing the property asks for with.** For `AssociatedRoles` that call
//! is `AddRoleToDBCluster(DBClusterIdentifier, RoleArn, FeatureName)` — the one Amazon RDS
//! judges the role's trust policy and the engine version's published feature names in — so a
//! stack that declares an association reaches a complete status with that association reported
//! on the cluster, whether the declaration carried it from the start or an update added it to a
//! cluster that had none, and a client reading the cluster back through `DescribeDBClusters`
//! finds it under `AssociatedRoles` with the status an association that holds reports.
//!
//! The DB INSTANCE side of the same rule is
//! `cloudformation_rds_db_instance_associated_roles`, which also states what a refusal of the
//! association does to the resource.
//!
//! Three crates have to agree for this to hold: CloudFormation deploys and updates the stack,
//! Amazon RDS owns the cluster and its associations, and IAM answers for the role and its
//! trust policy — so the suite lives here rather than in any one of them.
//!
//! The rows span what the rule generalises over: the engine family the cluster runs and the
//! `FeatureName` that family publishes for an association (`DescribeDBEngineVersions`
//! `SupportedFeatureNames`, which is where a client reads them) — an S3 feature of Aurora
//! PostgreSQL and a machine learning integration of Aurora MySQL — and WHEN the declaration
//! asks for it.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_of, blocks_of, element, elements, form_encode, opt_element};

/// The Region every request here is signed for, and so the Region the estate lives in.
const REGION: &str = "us-west-2";

/// The master user every cluster below is created with, and its password — test credentials,
/// and the only place in this suite they appear.
const MASTER_USER: &str = "emuadmin";
const MASTER_PASSWORD: &str = "emu-Secret-99";

/// The service principal a role Amazon RDS acts under for a DB cluster feature must trust.
const RDS_PRINCIPAL: &str = "rds.amazonaws.com";

/// The `Status` an association that holds reports.
const ACTIVE: &str = "ACTIVE";

/// The statuses a stack settles in after a create and after an update every owning service
/// accepted.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";

/// The logical ids in every template below.
const DB_CLUSTER: &str = "DbCluster";
const DB_CLUSTER_ROLE: &str = "ClusterRole";

/// How long one poll waits before the next, and how many are made: every transition waited
/// for here is owed on another service's own clock, so the suite waits for the CONDITION
/// rather than for a span, with a budget far longer than the spans themselves so a loaded
/// host does not decide the outcome.
const POLL: Duration = Duration::from_millis(50);
/// See [`POLL`].
const POLLS: usize = 2_400;

/// The emulator as it is ASSEMBLED, on one engine (`aws_emulator_router::build_router`): the
/// CloudFormation provisioner's own calls are dispatched back into this same router, so the
/// cluster it creates and the role it associates are the ones this stack deployed.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request and what each handler reads its Region from.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One awsQuery call of `service`, answered with the HTTP status and the XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One CloudFormation call over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "cloudformation", "2010-05-15", action, members).await
}

/// One row of the table: the cluster's engine and the feature the association it is later
/// given is for.
struct Row {
    case: &'static str,
    /// The stack the UPDATE test deploys, and the prefix every resource of it is named under.
    stack: &'static str,
    /// The stack the CREATE test deploys, which declares the association from the start.
    created_with_the_role_stack: &'static str,
    /// `Engine`: the family the cluster runs.
    engine: &'static str,
    /// A `FeatureName` that family publishes.
    feature_name: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "an S3 feature of the Aurora PostgreSQL family",
        stack: "xsvc-rds-cluster-roles-pg",
        created_with_the_role_stack: "xsvc-rds-cluster-roles-pg-create",
        engine: "aurora-postgresql",
        feature_name: "s3Import",
    },
    Row {
        case: "a machine learning integration of the Aurora MySQL family",
        stack: "xsvc-rds-cluster-roles-mysql",
        created_with_the_role_stack: "xsvc-rds-cluster-roles-mysql-create",
        engine: "aurora-mysql",
        feature_name: "Bedrock",
    },
];

/// The template one row deploys under `stack`: the role the association names, and the cluster
/// — which declares the association only once `associated` says so.
fn template(row: &Row, stack: &str, associated: bool) -> String {
    let mut properties = json!({
        "DBClusterIdentifier": format!("{stack}-cluster"),
        "Engine": row.engine,
        "MasterUsername": MASTER_USER,
        "MasterUserPassword": MASTER_PASSWORD,
    });
    if associated {
        properties["AssociatedRoles"] = json!([{
            "RoleArn": { "Fn::GetAtt": [DB_CLUSTER_ROLE, "Arn"] },
            "FeatureName": row.feature_name,
        }]);
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "a DB cluster and the IAM role one of its features acts under",
        "Resources": {
            DB_CLUSTER_ROLE: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": format!("{stack}-role"),
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": { "Service": RDS_PRINCIPAL },
                            "Action": "sts:AssumeRole",
                        }],
                    },
                },
            },
            DB_CLUSTER: {
                "Type": "AWS::RDS::DBCluster",
                "Properties": properties,
            },
        },
        "Outputs": {
            "DbClusterIdentifier": { "Value": { "Ref": DB_CLUSTER } },
        },
    })
    .to_string()
}

/// The `StackStatus` the stack reports now.
async fn stack_status(router: &MultiServiceRouter, stack: &str) -> String {
    let (status, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "DescribeStacks {stack:?}: expected HTTP 200, got {status} with body {body}"
    );
    opt_element(&body, "StackStatus").unwrap_or_default()
}

/// Poll `stack` until the operation in flight has finished, in whatever status it finished in:
/// the transitions on the way are owed on the stack's own clock, so this waits for the
/// condition rather than for a span.
async fn wait_past_the_operation(router: &MultiServiceRouter, stack: &str) -> String {
    let mut last = stack_status(router, stack).await;
    for _ in 0..POLLS {
        if !last.ends_with("_IN_PROGRESS") {
            return last;
        }
        tokio::time::sleep(POLL).await;
        last = stack_status(router, stack).await;
    }
    last
}

/// Every `ResourceStatusReason` the stack has recorded, so an operation that did not settle
/// where the rule says reports WHY rather than only that it did not.
async fn failure_reasons(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let (_, events) = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    elements(&events, "ResourceStatusReason")
}

/// The `(RoleArn, FeatureName, Status)` triples Amazon RDS reports on the DB cluster
/// `identifier`, read one per `AssociatedRoles` entry.
async fn associated_roles(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
) -> Vec<(String, String, String)> {
    let (status, body) = query(
        router,
        "rds",
        "2014-10-31",
        "DescribeDBClusters",
        &[("DBClusterIdentifier", identifier)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected Amazon RDS to resolve {identifier:?}, got {status} with {body}"
    );
    let Some(listed) = block_of(&body, "AssociatedRoles") else {
        return Vec::new();
    };
    blocks_of(listed, "DBClusterRole")
        .into_iter()
        .map(|entry| {
            (
                element(entry, "RoleArn"),
                element(entry, "FeatureName"),
                element(entry, "Status"),
            )
        })
        .collect()
}

/// The ARN IAM reports for the role `name`, which is what the association has to name.
async fn role_arn(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, body) = query(
        router,
        "iam",
        "2010-05-08",
        "GetRole",
        &[("RoleName", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected IAM to hold the role {name:?}, got {status} with {body}"
    );
    element(&body, "Arn")
}

/// Every row: a cluster whose declaration carries the association FROM THE START reports it
/// once the stack's create is complete — the create path of the same rule, and the one that
/// proves the association is made exactly once (a second attempt at one Amazon RDS already
/// holds is a refusal that would fail the resource).
#[tokio::test]
async fn a_cfn_db_cluster_carries_the_role_association_its_create_declares() {
    for row in ROWS {
        let case = row.case;
        let router = build_router();
        // A stack of its own, so this suite's two tests cannot see each other's resources.
        let stack = row.created_with_the_role_stack;
        let identifier = format!("{stack}-cluster");

        let body = template(row, stack, true);
        let (status, answered) = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", stack),
                ("TemplateBody", &body),
                ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {answered}"
        );
        let settled = wait_past_the_operation(&router, stack).await;
        assert_eq!(
            settled, CREATE_COMPLETE,
            "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {settled:?} with \
             resource failures {:?}",
            failure_reasons(&router, stack).await
        );

        let name = format!("{stack}-role");
        let wanted = vec![(
            role_arn(&router, case, &name).await,
            row.feature_name.to_string(),
            ACTIVE.to_string(),
        )];
        let reported = associated_roles(&router, case, &identifier).await;
        assert_eq!(
            reported, wanted,
            "case {case:?}: expected the cluster's associations {wanted:?}, got {reported:?}"
        );
    }
}

/// Every row: the cluster deploys with no association, the stack update that declares one adds
/// it in Amazon RDS, and the cluster reports exactly that association.
#[tokio::test]
async fn a_cfn_db_cluster_takes_the_role_association_an_update_declares() {
    for row in ROWS {
        let case = row.case;
        let router = build_router();
        let stack = row.stack;
        let identifier = format!("{stack}-cluster");

        let body = template(row, stack, false);
        let (status, answered) = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", stack),
                ("TemplateBody", &body),
                ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {answered}"
        );
        let settled = wait_past_the_operation(&router, stack).await;
        assert_eq!(
            settled, CREATE_COMPLETE,
            "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {settled:?} with \
             resource failures {:?}",
            failure_reasons(&router, stack).await
        );
        let reported = associated_roles(&router, case, &identifier).await;
        assert!(
            reported.is_empty(),
            "case {case:?}: expected a cluster declared without associations to report none, got \
             {reported:?}"
        );

        // The one change the update declares is the association, which `ModifyDBCluster` takes
        // no member for: what applies it is the call that associates a role.
        let updated = template(row, stack, true);
        let (status, answered) = cloudformation(
            &router,
            "UpdateStack",
            &[
                ("StackName", stack),
                ("TemplateBody", &updated),
                ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with body {answered}"
        );
        let settled = wait_past_the_operation(&router, stack).await;
        assert_eq!(
            settled, UPDATE_COMPLETE,
            "case {case:?}: expected the stack to reach {UPDATE_COMPLETE}, got {settled:?} with \
             resource failures {:?}",
            failure_reasons(&router, stack).await
        );

        let name = format!("{stack}-role");
        let wanted = vec![(
            role_arn(&router, case, &name).await,
            row.feature_name.to_string(),
            ACTIVE.to_string(),
        )];
        let reported = associated_roles(&router, case, &identifier).await;
        assert_eq!(
            reported, wanted,
            "case {case:?}: expected the cluster's associations {wanted:?}, got {reported:?}"
        );
    }
}
