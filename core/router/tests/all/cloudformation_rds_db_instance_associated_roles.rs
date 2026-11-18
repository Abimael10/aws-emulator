// SPDX-License-Identifier: CC0-1.0

//! The IAM role associations a CloudFormation-deployed `AWS::RDS::DBInstance` declares.
//!
//! The rule under test: **a declared property that is no member of the owning service's
//! CREATE is carried to that service by the call that DOES take it, so the resource the stack
//! reports created really carries it — and a refusal of that call is the resource's own
//! failure, reported in the owning service's words.** For `AWS::RDS::DBInstance`'s
//! `AssociatedRoles` that call is `AddRoleToDBInstance(DBInstanceIdentifier, RoleArn,
//! FeatureName)`, which Amazon RDS takes only once the instance is `available`: so a stack
//! declaring role associations reaches `CREATE_COMPLETE` with every one of them reported on
//! the instance, and a stack whose declaration Amazon RDS will not associate — a role whose
//! trust policy does not let that service assume it, a feature its engine does not publish —
//! fails the DB instance with that service's own sentence and rolls back.
//!
//! Three crates have to agree for that to hold: CloudFormation deploys the estate and holds
//! the instance's creation open until Amazon RDS reports it up, Amazon RDS owns the instance
//! and the associations, and IAM answers for the role and its trust policy — so the suite
//! lives here rather than in any one of them.
//!
//! The rows span what the rule generalises over: how MANY associations the declaration asks
//! for (none, one, two), and which of the two refusals Amazon RDS answers. The role ARN each
//! association names is resolved from the role the same stack declares
//! (`Fn::GetAtt [.., "Arn"]`), so the association is also what proves the handle the stack
//! published addresses the role IAM really holds.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_of, blocks_of, element, elements, form_encode, opt_element};

/// The Region every request here is signed for, and so the Region the estate lives in.
const REGION: &str = "us-west-2";

/// The master user every DB instance below is created with, and its password — test
/// credentials, and the only place in this suite they appear.
const MASTER_USER: &str = "emuadmin";
const MASTER_PASSWORD: &str = "emu-Secret-99";

/// The engine every instance runs, and the class and storage it is offered with. RDS for
/// PostgreSQL is the family whose published `SupportedFeatureNames` carry the S3 features an
/// association names (`DescribeDBEngineVersions`, which is where a client reads them).
const ENGINE: &str = "postgres";
const INSTANCE_CLASS: &str = "db.t3.micro";
const ALLOCATED_STORAGE: &str = "20";

/// The service principal a role Amazon RDS acts under for a DB instance feature must trust,
/// and a principal of another service, which it must not accept in its place.
const RDS_PRINCIPAL: &str = "rds.amazonaws.com";
const OTHER_PRINCIPAL: &str = "ec2.amazonaws.com";

/// The `Status` an association that holds reports.
const ACTIVE: &str = "ACTIVE";

/// The statuses a stack settles in: the one a create every owning service accepted reaches,
/// and the one a create whose rolled-back resources were all removed again reaches.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";

/// The `ResourceStatus` a resource whose create failed reports.
const RESOURCE_CREATE_FAILED: &str = "CREATE_FAILED";

/// The `HandlerErrorCode` class CloudFormation reports a refusal of the request a resource's
/// declaration asked for under, which the resource's status reason names beside the owning
/// service's own message.
const HANDLER_INVALID_REQUEST: &str = "HandlerErrorCode: InvalidRequest";

/// The logical id of the DB instance in every template below.
const DB_INSTANCE: &str = "DbInstance";

/// What Amazon RDS answers a role whose trust policy does not let it assume the role: ONE
/// fixed sentence, spelled out here rather than taken from the code under test, so this table
/// pins the wording the stack's failure reason carries.
const ROLE_NOT_TRUSTED: &str = "IAM role ARN value is invalid or does not include the \
                                required permissions for: AWS_ROLE_INTEGRATION";

/// How long one poll waits before the next, and how many are made: every transition waited
/// for here is owed on another service's own clock, so the suite waits for the CONDITION
/// rather than for a span, with a budget far longer than the spans themselves so a loaded
/// host does not decide the outcome.
const POLL: Duration = Duration::from_millis(50);
/// See [`POLL`].
const POLLS: usize = 2_400;

/// The emulator as it is ASSEMBLED, on one engine (`aws_emulator_router::build_router`): the
/// CloudFormation provisioner's own calls are dispatched back into this same router, so the
/// instance it creates and the role it associates are the ones this stack deployed.
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

/// One association an `AWS::RDS::DBInstance` declares: the feature it is for, and the service
/// principal the role it names trusts.
struct DeclaredRole {
    /// The logical id the role is declared under, and the suffix of its name.
    logical_id: &'static str,
    /// `FeatureName`: "the name of the feature associated with the ... IAM role".
    feature_name: &'static str,
    /// The service principal the role's own trust policy admits.
    trusts: &'static str,
}

/// One row of the table: the associations one stack's DB instance declares, and what the
/// deployment must then do.
struct Row {
    case: &'static str,
    /// The stack, and the prefix every resource of this row is named under.
    stack: &'static str,
    /// The associations the DB instance declares, in the order the template states them.
    roles: &'static [DeclaredRole],
    /// The `StackStatus` the deployment has to settle in.
    settles_in: &'static str,
    /// The sentence the DB instance's `CREATE_FAILED` reason has to carry, for a row whose
    /// declaration Amazon RDS will not associate. `None` is a row that must be associated.
    refused_with: Option<&'static str>,
}

const ROWS: &[Row] = &[
    Row {
        case: "no association at all",
        stack: "xsvc-rds-roles-none",
        roles: &[],
        settles_in: CREATE_COMPLETE,
        refused_with: None,
    },
    Row {
        case: "one role for a feature the engine publishes",
        stack: "xsvc-rds-roles-one",
        roles: &[DeclaredRole {
            logical_id: "ImportRole",
            feature_name: "s3Import",
            trusts: RDS_PRINCIPAL,
        }],
        settles_in: CREATE_COMPLETE,
        refused_with: None,
    },
    Row {
        case: "two roles, one per feature",
        stack: "xsvc-rds-roles-two",
        roles: &[
            DeclaredRole {
                logical_id: "ImportRole",
                feature_name: "s3Import",
                trusts: RDS_PRINCIPAL,
            },
            DeclaredRole {
                logical_id: "ExportRole",
                feature_name: "s3Export",
                trusts: RDS_PRINCIPAL,
            },
        ],
        settles_in: CREATE_COMPLETE,
        refused_with: None,
    },
    Row {
        case: "a role whose trust policy names another service's principal",
        stack: "xsvc-rds-roles-mistrusted",
        roles: &[DeclaredRole {
            logical_id: "ImportRole",
            feature_name: "s3Import",
            trusts: OTHER_PRINCIPAL,
        }],
        settles_in: ROLLBACK_COMPLETE,
        refused_with: Some(ROLE_NOT_TRUSTED),
    },
    Row {
        case: "a feature the instance's engine does not publish",
        stack: "xsvc-rds-roles-unpublished-feature",
        roles: &[DeclaredRole {
            logical_id: "ImportRole",
            // An Aurora machine learning integration, which RDS for PostgreSQL does not
            // publish in its `SupportedFeatureNames`.
            feature_name: "SageMaker",
            trusts: RDS_PRINCIPAL,
        }],
        settles_in: ROLLBACK_COMPLETE,
        refused_with: Some("The feature name SageMaker is not valid for the engine PostgreSQL."),
    },
];

/// The trust policy a declared role carries: `sts:AssumeRole` for the one service principal
/// the row states.
fn trust_policy(principal: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principal },
            "Action": "sts:AssumeRole",
        }],
    })
}

/// The template one row deploys: one `AWS::IAM::Role` per declared association, and the DB
/// instance that declares them — each `RoleArn` resolved off the role the stack itself
/// created.
fn template(row: &Row) -> String {
    let stack = row.stack;
    let mut resources = serde_json::Map::new();
    let mut associated = Vec::new();
    for role in row.roles {
        resources.insert(
            role.logical_id.to_string(),
            json!({
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": format!("{stack}-{}", role.logical_id),
                    "AssumeRolePolicyDocument": trust_policy(role.trusts),
                },
            }),
        );
        associated.push(json!({
            "RoleArn": { "Fn::GetAtt": [role.logical_id, "Arn"] },
            "FeatureName": role.feature_name,
        }));
    }
    let mut properties = json!({
        "DBInstanceIdentifier": format!("{stack}-db"),
        "Engine": ENGINE,
        "DBInstanceClass": INSTANCE_CLASS,
        "AllocatedStorage": ALLOCATED_STORAGE,
        "MasterUsername": MASTER_USER,
        "MasterUserPassword": MASTER_PASSWORD,
    });
    if !associated.is_empty() {
        properties["AssociatedRoles"] = Value::Array(associated);
    }
    resources.insert(
        DB_INSTANCE.to_string(),
        json!({ "Type": "AWS::RDS::DBInstance", "Properties": properties }),
    );
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "a DB instance and the IAM roles its features act under",
        "Resources": Value::Object(resources),
        "Outputs": {
            "DbInstanceIdentifier": { "Value": { "Ref": DB_INSTANCE } },
        },
    })
    .to_string()
}

/// The `StackStatus` the stack reports now, with the answer it was read from.
async fn stack_status(router: &MultiServiceRouter, stack: &str) -> (String, String) {
    let (status, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "DescribeStacks {stack:?}: expected HTTP 200, got {status} with body {body}"
    );
    (opt_element(&body, "StackStatus").unwrap_or_default(), body)
}

/// Poll `stack` until the create has finished, in whatever status it finished in — which is
/// what each row says it must settle in: the transitions on the way are owed on the stack's
/// own clock, so this waits for the condition rather than for a span.
async fn wait_past_the_create(router: &MultiServiceRouter, stack: &str) -> (String, String) {
    let mut last = stack_status(router, stack).await;
    for _ in 0..POLLS {
        if !last.0.ends_with("_IN_PROGRESS") {
            return last;
        }
        tokio::time::sleep(POLL).await;
        last = stack_status(router, stack).await;
    }
    last
}

/// The `ResourceStatusReason` the stack's EVENTS carry for the `CREATE_FAILED` of one
/// logical resource, read from the event that carries both so the reason cannot be read off
/// another resource's event.
///
/// The EVENTS are where a failed create's reason stays readable: a rolled-back create removes
/// the resource again, so what the stack reports for it afterwards is its own removal.
async fn failed_create_reason(
    router: &MultiServiceRouter,
    stack: &str,
    logical_id: &str,
) -> (Option<String>, String) {
    let (status, body) =
        cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "DescribeStackEvents {stack:?}: expected HTTP 200, got {status} with body {body}"
    );
    let found = blocks_of(&body, "member")
        .into_iter()
        .find(|member| {
            element(member, "LogicalResourceId") == logical_id
                && element(member, "ResourceStatus") == RESOURCE_CREATE_FAILED
        })
        .map(|member| {
            elements(member, "ResourceStatusReason")
                .into_iter()
                .next()
                .unwrap_or_default()
        });
    (found, body.clone())
}

/// Every `ResourceStatusReason` the stack has recorded, so a deployment that did not settle
/// where the row says reports WHY rather than only that it did not.
async fn failure_reasons(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let (_, events) = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    elements(&events, "ResourceStatusReason")
}

/// The `(RoleArn, FeatureName, Status)` triples Amazon RDS reports on the DB instance
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
        "DescribeDBInstances",
        &[("DBInstanceIdentifier", identifier)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected Amazon RDS to resolve {identifier:?}, got {status} with {body}"
    );
    let Some(listed) = block_of(&body, "AssociatedRoles") else {
        return Vec::new();
    };
    blocks_of(listed, "DBInstanceRole")
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

/// Every row: the stack settles where the row says, and the associations the declaration
/// asked for are exactly the ones Amazon RDS reports on the instance.
#[tokio::test]
async fn a_cfn_db_instance_carries_the_role_associations_it_declares() {
    for row in ROWS {
        let case = row.case;
        let router = build_router();
        let stack = row.stack;

        let body = template(row);
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

        let (settled, _) = wait_past_the_create(&router, stack).await;
        assert_eq!(
            settled, row.settles_in,
            "case {case:?}: expected the stack to settle in {:?}, got {settled:?} with resource \
             failures {:?}",
            row.settles_in,
            failure_reasons(&router, stack).await
        );

        let identifier = format!("{stack}-db");
        match row.refused_with {
            // A declaration Amazon RDS will not associate: the DB instance's own create
            // failed, carrying that service's sentence, and the rollback took the instance
            // away again — so the stack publishes no database with a half-made association.
            Some(refused) => {
                let (reason, events) = failed_create_reason(&router, stack, DB_INSTANCE).await;
                let reason = reason.unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: expected a {RESOURCE_CREATE_FAILED} event for \
                         {DB_INSTANCE}, got the events {events}"
                    )
                });
                assert!(
                    reason.contains(refused) && reason.contains(HANDLER_INVALID_REQUEST),
                    "case {case:?}: expected the DB instance's reason to carry {refused:?} and \
                     {HANDLER_INVALID_REQUEST:?}, got {reason:?}"
                );
                let (status, body) = query(
                    &router,
                    "rds",
                    "2014-10-31",
                    "DescribeDBInstances",
                    &[("DBInstanceIdentifier", &identifier)],
                )
                .await;
                assert_eq!(
                    status, 404,
                    "case {case:?}: expected Amazon RDS to hold no instance {identifier:?} after \
                     the rollback, got {status} with {body}"
                );
            }
            // Every declared association is in place, naming the role IAM really holds and
            // reporting the status an association that holds carries.
            None => {
                let reported = associated_roles(&router, case, &identifier).await;
                let mut wanted = Vec::new();
                for role in row.roles {
                    let name = format!("{stack}-{}", role.logical_id);
                    wanted.push((
                        role_arn(&router, case, &name).await,
                        role.feature_name.to_string(),
                        ACTIVE.to_string(),
                    ));
                }
                assert_eq!(
                    reported, wanted,
                    "case {case:?}: expected the instance's associations {wanted:?}, got \
                     {reported:?}"
                );
            }
        }
    }
}
