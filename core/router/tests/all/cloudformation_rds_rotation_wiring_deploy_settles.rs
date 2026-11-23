// SPDX-License-Identifier: CC0-1.0

//! The CloudFormation deploy of an RDS credential-rotation estate — a DB instance, the Secrets
//! Manager secret holding its master credential, the rotation function with its execution role,
//! the grant that lets Secrets Manager invoke it and the schedule binding the two — always
//! SETTLES, so a caller polling `DescribeStacks` finishes.
//!
//! The rule under test: **a create reaches a TERMINAL stack status whatever the owning services
//! answer for the resources it declares — `CREATE_COMPLETE` once every one of them was created,
//! and `ROLLBACK_COMPLETE` with a `CREATE_FAILED` event carrying the refusing service's own
//! reason on the resource it refused — so the stack never stays in `*_IN_PROGRESS` and a client
//! can tell a deploy that succeeded from one that rolled back.** A deploy that hangs is
//! indistinguishable from either: no stack status, no resource status and no failure reason is
//! observable, so an operator learns nothing about whether the rotation wiring was created.
//!
//! The rows differ along what the rule generalises over: WHICH part of the estate its owning
//! service can bring up and which service answers — the whole estate (every service accepts),
//! the schedule with no invoke grant on its function and the schedule over a secret nothing
//! holds (Secrets Manager refuses), and the DB instance placed in a subnet group nothing
//! declares (Amazon RDS refuses). Five crates have to agree for the rule to hold —
//! CloudFormation drives the deploy, Secrets Manager holds the secret and its rotation, Amazon
//! RDS owns the database, Lambda holds the function and its resource policy, IAM answers for the
//! execution role — so the suite lives here rather than in any one of them.
//!
//! What a completed deploy then makes READABLE is asserted from the owning services themselves:
//! the secret's `RotationEnabled`, `RotationLambdaARN` and `RotationRules` out of
//! `DescribeSecret`, the invoke grant out of the function's own policy, and the database out of
//! `DescribeDBInstances`.
//!
//! [`cloudformation_rds_credential_rotation_estate`](super::cloudformation_rds_credential_rotation_estate)
//! states the other rule about this estate — that a rotation PUTS the new credential in force in
//! Amazon RDS — with every row a deploy that completes. Neither suite's rows are the other's.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{
    element, elements, form_encode, members_of, opt_element, outputs_of, pairs_of,
};

/// The Region every request here is signed for, and so the Region the estate lives in.
const REGION: &str = "us-west-2";

/// The username the secret is generated around, and the key its generated password lands under.
const USERNAME: &str = "dbadmin";
/// See [`USERNAME`].
const PASSWORD_KEY: &str = "password";

/// How long one poll waits before the next, and how many are made: every transition waited for
/// here is owed on another service's own clock, so the suite waits for the CONDITION rather than
/// for a span, with a budget far longer than the spans themselves so a loaded host does not
/// decide the outcome.
const POLL: Duration = Duration::from_millis(50);
/// See [`POLL`].
const POLLS: usize = 2_400;

/// The terminal `StackStatus` of a create every owning service accepted, and of one a service
/// refused a resource of: "if the resource creation fails, the resources it made are removed
/// again and the stack settles into ROLLBACK_COMPLETE".
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
/// See [`CREATE_COMPLETE`].
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";

/// The `ResourceStatus` every resource of a completed create reports, and the one the resource a
/// service refused reports in the stack's events.
const RESOURCE_CREATE_COMPLETE: &str = "CREATE_COMPLETE";
/// See [`RESOURCE_CREATE_COMPLETE`].
const RESOURCE_CREATE_FAILED: &str = "CREATE_FAILED";

/// The suffix every non-terminal stack status carries, which is what the rule says a settled
/// create does not report.
const IN_PROGRESS: &str = "_IN_PROGRESS";

/// The `DBInstanceStatus` of an instance that serves connections.
const AVAILABLE: &str = "available";

/// The service principal Secrets Manager invokes a rotation function as ("ensure that the
/// function policy grants access to the principal `secretsmanager.amazonaws.com`", Secrets
/// Manager User Guide, *Permissions for rotation*), and the action a rotation needs on it ("you
/// also need `lambda:InvokeFunction` permissions on the rotation function", `RotateSecret`, the
/// Secrets Manager Smithy model).
const SERVICE_PRINCIPAL: &str = "secretsmanager.amazonaws.com";
/// See [`SERVICE_PRINCIPAL`].
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// How often the schedule asks for the credential to be rotated, as the declaration states it.
const ROTATION_DAYS: u64 = 7;

/// How long one invocation of the rotation function is given, as the function's own `Timeout`:
/// well past what a run needs, because what is under test is whether the DEPLOY settles and not
/// how fast an interpreter gets through a handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// The rotation handler the estate deploys. Every row holds the first rotation back
/// (`RotateImmediatelyOnUpdate` false), so what reaches this function is the configuration test
/// Secrets Manager runs after the call it answers — "Secrets Manager tests the rotation
/// configuration by running the testSecret step of the Lambda rotation function"
/// (`RotateSecretRequest$RotateImmediately`, that service's Smithy model) — and the credential
/// the rotation itself writes is the sibling suite's own assertion.
const ROTATION_FUNCTION: &str = r#"
def lambda_handler(event, context):
    return {"step": event["Step"]}
"#;

/// The emulator as it is ASSEMBLED, on one engine (`aws_emulator_router::build_router`), so the
/// calls CloudFormation makes while it deploys reach the same services this suite reads.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the credential
    // scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request and what each handler reads its Region from.
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
    let response = Box::pin(router.dispatch(request)).await;
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

/// One AWS Secrets Manager call over its awsJson1_1 wire.
async fn secretsmanager(
    router: &MultiServiceRouter,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("secretsmanager.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("secretsmanager.{operation}"))
        .header("authorization", authorization("secretsmanager"))
        .body(Bytes::from(request.to_string()))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(http_request)).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// One Lambda `restJson1` read, as its status and its parsed body.
async fn lambda_get(router: &MultiServiceRouter, uri: &str) -> (u16, Value) {
    let request = http::Request::builder()
        .method("GET")
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body())
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(response.body()).to_string()));
    (status, parsed)
}

/// One row of the table: the estate the stack declares, and the terminal status the rule says its
/// create must settle in.
struct Row {
    case: &'static str,
    /// The stack, and the prefix every resource of this row is named under.
    stack: &'static str,
    /// Whether the template declares the `AWS::Lambda::Permission` that grants Secrets Manager
    /// the invoke on the rotation function.
    grants_invoke: bool,
    /// The secret the schedule binds the rotation to: `None` for the stack's own secret, `Some`
    /// for a name no secret in the account carries.
    schedule_secret: Option<&'static str>,
    /// A `DBSubnetGroupName` for the DB instance: `None` leaves the property out, `Some` names a
    /// group nothing declares, which Amazon RDS resolves before it will create an instance.
    db_subnet_group: Option<&'static str>,
    /// The terminal `StackStatus` the create must reach.
    settles_to: &'static str,
    /// The logical id the create must report `CREATE_FAILED` on, for an estate a service refuses
    /// a resource of; `None` for one every service accepts.
    fails_on: Option<&'static str>,
    /// The service CloudFormation reports the refusal from, as its SDK words the diagnostic the
    /// reason carries (`(Service: <name>, …)`) — so the reason is pinned to the service that
    /// refused and not merely to being present.
    refused_by: Option<&'static str>,
    /// What the refusal's own message must carry: a value this row's declaration states, or the
    /// fixed text of the modelled message where it names no value of the request.
    reason_carries: Option<&'static str>,
}

/// Every logical id the template declares, which a completed create must report created.
const DECLARED: &[&str] = &[
    "DbSecret",
    "DbInstance",
    "RotationRole",
    "RotationFn",
    "InvokePermission",
    "SecretRotation",
];

const ROWS: &[Row] = &[
    Row {
        case: "the whole wiring, which every owning service accepts",
        stack: "xsvc-rotation-settles-complete",
        grants_invoke: true,
        schedule_secret: None,
        db_subnet_group: None,
        settles_to: CREATE_COMPLETE,
        fails_on: None,
        refused_by: None,
        reason_carries: None,
    },
    Row {
        case: "a schedule whose function grants this service no invoke",
        stack: "xsvc-rotation-settles-no-grant",
        grants_invoke: false,
        schedule_secret: None,
        db_subnet_group: None,
        settles_to: ROLLBACK_COMPLETE,
        fails_on: Some("SecretRotation"),
        // "You also need lambda:InvokeFunction permissions on the rotation function"
        // (`RotateSecret`, the Secrets Manager Smithy model): a rotation that service cannot
        // invoke the function for is refused, and its message names the principal whose grant
        // the template left out.
        refused_by: Some("SecretsManager"),
        reason_carries: Some(SERVICE_PRINCIPAL),
    },
    Row {
        case: "a schedule over a secret nothing in the account holds",
        stack: "xsvc-rotation-settles-absent-secret",
        grants_invoke: true,
        schedule_secret: Some("xsvc-rotation-settles-absent-secret-nothing-holds-this"),
        db_subnet_group: None,
        settles_to: ROLLBACK_COMPLETE,
        fails_on: Some("SecretRotation"),
        // The modelled refusal of a secret that service does not hold, whose message names no
        // value of the request — so its own fixed text is what says which refusal this is.
        refused_by: Some("SecretsManager"),
        reason_carries: Some("can't find the specified secret"),
    },
    Row {
        case: "a database placed in a subnet group nothing declares",
        stack: "xsvc-rotation-settles-absent-subnet-group",
        grants_invoke: true,
        schedule_secret: None,
        db_subnet_group: Some("xsvc-rotation-settles-absent-subnet-group-nothing-declares-this"),
        settles_to: ROLLBACK_COMPLETE,
        fails_on: Some("DbInstance"),
        // "DBSubnetGroupName doesn't refer to an existing DB subnet group"
        // (`CreateDBInstance`, the Amazon RDS Smithy model), whose message names the group the
        // declaration asked for.
        refused_by: Some("Rds"),
        reason_carries: Some("xsvc-rotation-settles-absent-subnet-group-nothing-declares-this"),
    },
];

/// The template one row deploys: the generated secret, the DB instance whose master credential
/// comes out of it, the rotation function with its execution role, the invoke grant, and the
/// schedule that binds the function to the secret.
fn template(row: &Row) -> String {
    let stack = row.stack;
    let mut instance = json!({
        "DBInstanceIdentifier": format!("{stack}-db"),
        "Engine": "mysql",
        "DBInstanceClass": "db.t3.micro",
        "AllocatedStorage": "20",
        "MasterUsername": {
            "Fn::Sub": "{{resolve:secretsmanager:${DbSecret}:SecretString:username}}",
        },
        "MasterUserPassword": {
            "Fn::Sub": "{{resolve:secretsmanager:${DbSecret}:SecretString:password}}",
        },
    });
    if let Some(group) = row.db_subnet_group {
        instance["DBSubnetGroupName"] = json!(group);
    }

    // The grant has to be in force before the rotation this schedule configures reaches the
    // function, and the credential has to be the instance's, so the schedule is declared after
    // both — a row that declares no grant depends on what it does declare.
    let mut depends_on = vec![json!("DbInstance")];
    if row.grants_invoke {
        depends_on.push(json!("InvokePermission"));
    }
    let schedule_secret = match row.schedule_secret {
        Some(secret) => json!(secret),
        None => json!({ "Ref": "DbSecret" }),
    };

    let mut resources = Map::new();
    resources.insert(
        "DbSecret".to_string(),
        json!({
            "Type": "AWS::SecretsManager::Secret",
            "Properties": {
                "Name": format!("{stack}-credentials"),
                "GenerateSecretString": {
                    "SecretStringTemplate": json!({ "username": USERNAME }).to_string(),
                    "GenerateStringKey": PASSWORD_KEY,
                    "PasswordLength": 20,
                    "ExcludePunctuation": true,
                },
            },
        }),
    );
    resources.insert(
        "DbInstance".to_string(),
        json!({ "Type": "AWS::RDS::DBInstance", "Properties": instance }),
    );
    resources.insert(
        "RotationRole".to_string(),
        json!({
            "Type": "AWS::IAM::Role",
            "Properties": {
                "AssumeRolePolicyDocument": {
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": { "Service": "lambda.amazonaws.com" },
                        "Action": "sts:AssumeRole",
                    }],
                },
                "Policies": [{
                    "PolicyName": "rotate-the-credential",
                    "PolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Action": "secretsmanager:*",
                            "Resource": "*",
                        }],
                    },
                }],
            },
        }),
    );
    resources.insert(
        "RotationFn".to_string(),
        json!({
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "Runtime": "python3.12",
                "Handler": "index.lambda_handler",
                "Role": { "Fn::GetAtt": ["RotationRole", "Arn"] },
                "Timeout": FUNCTION_TIMEOUT,
                "Code": { "ZipFile": ROTATION_FUNCTION },
            },
        }),
    );
    if row.grants_invoke {
        resources.insert(
            "InvokePermission".to_string(),
            json!({
                "Type": "AWS::Lambda::Permission",
                "Properties": {
                    "Action": INVOKE_ACTION,
                    "FunctionName": { "Fn::GetAtt": ["RotationFn", "Arn"] },
                    "Principal": SERVICE_PRINCIPAL,
                },
            }),
        );
    }
    resources.insert(
        "SecretRotation".to_string(),
        json!({
            "Type": "AWS::SecretsManager::RotationSchedule",
            "DependsOn": depends_on,
            "Properties": {
                "SecretId": schedule_secret,
                "RotationLambdaARN": { "Fn::GetAtt": ["RotationFn", "Arn"] },
                "RotationRules": { "AutomaticallyAfterDays": ROTATION_DAYS },
                // The deploy configures the rotation without starting one, so what the stack
                // waits on is its own resources and not a rotation of the credential.
                "RotateImmediatelyOnUpdate": false,
            },
        }),
    );

    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "an RDS master credential and the schedule that rotates it",
        "Resources": Value::Object(resources),
        "Outputs": {
            "SecretArn": { "Value": { "Ref": "DbSecret" } },
            "DbInstanceIdentifier": { "Value": { "Ref": "DbInstance" } },
            "FunctionArn": { "Value": { "Fn::GetAtt": ["RotationFn", "Arn"] } },
            "FunctionName": { "Value": { "Ref": "RotationFn" } },
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

/// Poll `stack` until its create has finished, in whatever status it finished in — which is what
/// the rule says it must reach: the transitions on the way are owed on the stack's own clock, so
/// this waits for the CONDITION rather than for a span.
async fn wait_past_the_create(router: &MultiServiceRouter, stack: &str) -> (String, String) {
    let mut last = stack_status(router, stack).await;
    for _ in 0..POLLS {
        if !last.0.ends_with(IN_PROGRESS) {
            return last;
        }
        tokio::time::sleep(POLL).await;
        last = stack_status(router, stack).await;
    }
    last
}

/// Every `ResourceStatusReason` the stack has recorded, so a deploy that did not reach the status
/// the rule states reports WHY rather than only that it did not.
async fn failure_reasons(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let (_, events) = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    elements(&events, "ResourceStatusReason")
}

/// The reason the stack recorded against `logical_id` for the `CREATE_FAILED` of that resource,
/// read from the one event carrying both — never by zipping the two element lists, which a member
/// omitting either would misalign.
async fn create_failure_of(
    router: &MultiServiceRouter,
    stack: &str,
    logical_id: &str,
) -> Option<String> {
    let (_, events) = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    members_of(&events).into_iter().find_map(|event| {
        let failed = elements(event, "ResourceStatus")
            .into_iter()
            .next()
            .is_some_and(|status| status == RESOURCE_CREATE_FAILED);
        let named = elements(event, "LogicalResourceId")
            .into_iter()
            .next()
            .is_some_and(|named| named == logical_id);
        (failed && named)
            .then(|| elements(event, "ResourceStatusReason").into_iter().next())
            .flatten()
    })
}

/// Deploy `row`'s template and wait past its create, answering the status it settled in and the
/// `DescribeStacks` body it was read from.
async fn deploy(router: &MultiServiceRouter, row: &Row) -> (String, String) {
    let case = row.case;
    let body = template(row);
    let (status, answered) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", row.stack),
            ("TemplateBody", &body),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {answered}"
    );
    let (reached, described) = wait_past_the_create(router, row.stack).await;
    // The first half of the rule, stated on its own: whatever the owning services answered, the
    // create is over. A deploy still `*_IN_PROGRESS` here is the hang itself.
    assert!(
        !reached.ends_with(IN_PROGRESS),
        "case {case:?}: expected the create to leave {IN_PROGRESS}, got {reached:?} with the \
         resource failures {:?}",
        failure_reasons(router, row.stack).await
    );
    (reached, described)
}

/// RULE, for an estate every owning service accepts: the create settles `CREATE_COMPLETE`, every
/// declared resource reports created, and the rotation wiring is readable from the services that
/// own it.
#[tokio::test]
async fn the_rotation_wiring_deploy_settles_and_is_readable() {
    for row in ROWS.iter().filter(|row| row.fails_on.is_none()) {
        let case = row.case;
        let router = build_router();
        let (reached, described) = deploy(&router, row).await;

        let (_, resources) =
            cloudformation(&router, "DescribeStackResources", &[("StackName", row.stack)]).await;
        let reported = pairs_of(&resources, "LogicalResourceId", "ResourceStatus");
        assert_eq!(
            reached, row.settles_to,
            "case {case:?}: expected the create to settle {:?}, got {reached:?} with resources \
             {reported:?} and resource failures {:?}",
            row.settles_to,
            failure_reasons(&router, row.stack).await
        );
        for logical_id in DECLARED {
            let answered = reported.get(*logical_id).map(String::as_str);
            assert_eq!(
                answered,
                Some(RESOURCE_CREATE_COMPLETE),
                "case {case:?}: expected {logical_id} {RESOURCE_CREATE_COMPLETE}, got {answered:?} \
                 in {resources}"
            );
        }

        let outputs = outputs_of(&described);
        let read = |key: &str| {
            outputs
                .get(key)
                .cloned()
                .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {described}"))
        };
        let secret_arn = read("SecretArn");
        let identifier = read("DbInstanceIdentifier");
        let function_arn = read("FunctionArn");
        let function_name = read("FunctionName");

        // The rotation the schedule declared is the secret's own configuration in the service
        // that holds it.
        let (status, metadata) =
            secretsmanager(&router, "DescribeSecret", &json!({ "SecretId": &secret_arn })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeSecret, got {status} with {metadata}"
        );
        let rules = json!({ "AutomaticallyAfterDays": ROTATION_DAYS });
        for (member, wanted) in [
            ("RotationEnabled", json!(true)),
            ("RotationLambdaARN", json!(function_arn)),
            ("RotationRules", rules),
        ] {
            assert_eq!(
                metadata[member], wanted,
                "case {case:?}: expected the secret's {member} {wanted}, got {metadata}"
            );
        }

        // ...and the grant the stack declared is the function's own resource policy, so the
        // rotation this schedule starts can reach it.
        let (status, policy) =
            lambda_get(&router, &format!("/2015-03-31/functions/{function_name}/policy")).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetPolicy, got {status} with {policy}"
        );
        let document: Value = serde_json::from_str(policy["Policy"].as_str().unwrap_or_default())
            .unwrap_or(Value::Null);
        // `Action` is read in both the forms a policy document states one in — the action on its
        // own, and a one-element list of actions — so a correct grant is admitted either way.
        let allows = |stated: &Value, wanted: &str| match stated {
            Value::Array(actions) => actions.iter().any(|action| action == &json!(wanted)),
            other => other == &json!(wanted),
        };
        let granted = document["Statement"]
            .as_array()
            .map(|statements| {
                statements.iter().any(|statement| {
                    allows(&statement["Action"], INVOKE_ACTION)
                        && statement["Principal"]["Service"] == json!(SERVICE_PRINCIPAL)
                        && statement["Effect"] == json!("Allow")
                })
            })
            .unwrap_or_default();
        assert!(
            granted,
            "case {case:?}: expected the function's policy to allow {INVOKE_ACTION} to \
             {SERVICE_PRINCIPAL}, got {document}"
        );

        // ...and the database the credential belongs to is the one Amazon RDS serves, with the
        // master user the secret was generated around. Its status is read until that service
        // reports it available: the span it spends coming up is Amazon RDS's own.
        let mut instance = String::new();
        for _ in 0..POLLS {
            let (status, body) = query(
                &router,
                "rds",
                "2014-10-31",
                "DescribeDBInstances",
                &[("DBInstanceIdentifier", &identifier)],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected Amazon RDS to resolve {identifier:?}, got {status} with \
                 {body}"
            );
            instance = body;
            if element(&instance, "DBInstanceStatus") == AVAILABLE {
                break;
            }
            tokio::time::sleep(POLL).await;
        }
        for (member, wanted) in [("DBInstanceStatus", AVAILABLE), ("MasterUsername", USERNAME)] {
            let answered = element(&instance, member);
            assert_eq!(
                answered, wanted,
                "case {case:?}: expected the instance's {member} {wanted:?}, got {answered:?} in \
                 {instance}"
            );
        }
    }
}

/// RULE, for an estate whose owning service refuses a resource of it: the create still settles —
/// `ROLLBACK_COMPLETE`, with a `CREATE_FAILED` event on the resource that was refused carrying
/// that service's own reason — so the deploy is distinguishable from one that completed.
#[tokio::test]
async fn a_refused_rotation_wiring_deploy_settles_rolled_back() {
    for row in ROWS.iter().filter(|row| row.fails_on.is_some()) {
        let case = row.case;
        let router = build_router();
        let (reached, _) = deploy(&router, row).await;

        assert_eq!(
            reached, row.settles_to,
            "case {case:?}: expected the create to settle {:?}, got {reached:?} with the resource \
             failures {:?}",
            row.settles_to,
            failure_reasons(&router, row.stack).await
        );
        let failed_on = row.fails_on.unwrap_or_default();
        let reason = create_failure_of(&router, row.stack, failed_on).await;
        let reported = reason.clone().unwrap_or_default();
        assert!(
            !reported.trim().is_empty(),
            "case {case:?}: expected a {RESOURCE_CREATE_FAILED} reason on {failed_on}, got \
             {reason:?} among the stack's reasons {:?}",
            failure_reasons(&router, row.stack).await
        );
        // The reason is the REFUSING service's own: the diagnostic naming that service, and the
        // value of this row's declaration its message carries (or that message's own fixed text
        // where it names no value of the request). A generic reason, or the right status reached
        // for the wrong cause, fails here.
        if let Some(service) = row.refused_by {
            let diagnostic = format!("(Service: {service}");
            assert!(
                reported.contains(&diagnostic),
                "case {case:?}: expected the reason on {failed_on} to carry {diagnostic:?} from \
                 the service that refused, got {reported:?}"
            );
        }
        if let Some(carried) = row.reason_carries {
            assert!(
                reported.contains(carried),
                "case {case:?}: expected the reason on {failed_on} to carry {carried:?}, got \
                 {reported:?}"
            );
        }
    }
}
