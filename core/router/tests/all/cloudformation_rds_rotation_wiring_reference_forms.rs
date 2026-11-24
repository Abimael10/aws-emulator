// SPDX-License-Identifier: CC0-1.0

//! The CloudFormation deploy of an RDS credential-rotation estate — a DB instance, the Secrets
//! Manager secret holding its master credential, the rotation function with its execution role,
//! the grant that lets Secrets Manager invoke it and the schedule binding the two — reaches
//! `CREATE_COMPLETE` and leaves the rotation wiring readable WHICHEVER published form the
//! declarations name each other by.
//!
//! The rule under test: **a create resolves every reference a declaration makes to the
//! identifier the referenced resource's own return value publishes, and hands the declared
//! properties to the owning service as the members of that service's own call, so a template is
//! deployed on the strength of what it declares and not of the spelling it declares it in — a
//! value the owning service refuses being the resource's `CREATE_FAILED` and the stack's
//! rollback.** A deploy that only works for one spelling of a reference makes the documented
//! reference pattern undeployable for every template that writes it the other way, while
//! reporting nothing about why.
//!
//! The rows differ along what the rule generalises over: the FORM each declaration names the
//! resources it wires together in — the secret the schedule rotates named by `Ref` (which
//! "returns the ARN of the secret"), by the `Id` attribute the type publishes, and by the name
//! the secret was created under (`SecretId` being "the ARN or name of the secret",
//! `RotateSecret`, the Secrets Manager Smithy model); the function the invoke grant is put on
//! named by `Fn::GetAtt … Arn` and by `Ref` (which "returns the function name") — and along the
//! form and the value of the schedule itself, which the type states either as an interval in
//! days or as a `rate()`/`cron()` expression with an optional rotation window. The interval is
//! driven to each published bound of its documented range and one past it
//! (`RotationRulesType$AutomaticallyAfterDays`, range 1..1000).
//!
//! Five crates have to agree for the rule to hold — CloudFormation drives the deploy, Secrets
//! Manager holds the secret and its rotation, Amazon RDS owns the database, Lambda holds the
//! function and its resource policy, IAM answers for the execution role — so the suite lives
//! here rather than in any one of them.
//!
//! [`cloudformation_rds_rotation_wiring_deploy_settles`](super::cloudformation_rds_rotation_wiring_deploy_settles)
//! states the other rule about this estate — that the create always SETTLES, whichever of the
//! owning services refuses which resource — with every row writing its references ONE way.
//! Neither suite's rows are the other's.

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
/// refused a resource of.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
/// See [`CREATE_COMPLETE`].
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";

/// The `ResourceStatus` every resource of a completed create reports, and the one the resource a
/// service refused reports in the stack's events.
const RESOURCE_CREATE_COMPLETE: &str = "CREATE_COMPLETE";
/// See [`RESOURCE_CREATE_COMPLETE`].
const RESOURCE_CREATE_FAILED: &str = "CREATE_FAILED";

/// The suffix every non-terminal stack status carries, which is what a settled create does not
/// report.
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

/// The SDK name CloudFormation words a Secrets Manager refusal under in a resource's
/// `ResourceStatusReason` (`(Service: SecretsManager, …)`), which is what pins a rollback to the
/// service that refused rather than merely to having failed.
const SECRETSMANAGER_SDK_NAME: &str = "SecretsManager";

/// How long one invocation of the rotation function is given, as the function's own `Timeout`:
/// well past what a run needs, because what is under test is whether the DEPLOY completes and
/// not how fast an interpreter gets through a handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// The rotation handler the estate deploys. Every row holds the first rotation back
/// (`RotateImmediatelyOnUpdate` false), so what reaches this function is the configuration test
/// Secrets Manager runs after the call it answers — "Secrets Manager tests the rotation
/// configuration by running the testSecret step of the Lambda rotation function"
/// (`RotateSecretRequest$RotateImmediately`, that service's Smithy model).
const ROTATION_FUNCTION: &str = r#"
def lambda_handler(event, context):
    return {"step": event["Step"]}
"#;

/// Every logical id the template declares, which a completed create must report created.
const DECLARED: &[&str] = &[
    "DbSecret",
    "DbInstance",
    "RotationRole",
    "RotationFn",
    "InvokePermission",
    "SecretRotation",
];

/// The form the rotation schedule's `SecretId` names the secret in. Each is a published handle
/// for the same secret: `Ref` on an `AWS::SecretsManager::Secret` "returns the ARN of the
/// secret", that type publishes `Id` as its one `Fn::GetAtt` attribute (the same ARN), and
/// `RotateSecret$SecretId` is "the ARN or name of the secret to rotate".
#[derive(Debug, Clone, Copy)]
enum SecretReference {
    /// `{ "Ref": "DbSecret" }`.
    Ref,
    /// `{ "Fn::GetAtt": ["DbSecret", "Id"] }`.
    PublishedId,
    /// The `Name` the declaration created the secret under, written out.
    CreatedName,
}

/// The form the invoke grant's `FunctionName` names the rotation function in. Both are published
/// handles for the same function: `Ref` on an `AWS::Lambda::Function` "returns the function
/// name", that type publishes `Arn`, and `AddPermission$FunctionName` takes "the name or ARN of
/// the Lambda function".
#[derive(Debug, Clone, Copy)]
enum FunctionReference {
    /// `{ "Fn::GetAtt": ["RotationFn", "Arn"] }`.
    Arn,
    /// `{ "Ref": "RotationFn" }`.
    Name,
}

/// The `RotationRules` the schedule declares. The type states a schedule either way and they are
/// alternatives: "you can set the rotation schedule in `RotationRules` with
/// `AutomaticallyAfterDays` or `ScheduleExpression`, but not both"
/// (`RotationRulesType$AutomaticallyAfterDays`, the Secrets Manager Smithy model).
#[derive(Debug, Clone, Copy)]
enum Rules {
    /// An interval in days (`AutomaticallyAfterDays`, range 1..1000).
    Days(u64),
    /// A `rate()` or `cron()` expression, with the rotation window `Duration` where the row
    /// states one.
    Schedule(&'static str, Option<&'static str>),
}

impl Rules {
    /// The `RotationRules` document the declaration carries, which is also what the secret must
    /// report back: every member a declaration states reaches `RotateSecret` under its own name.
    fn declared(self) -> Value {
        match self {
            Self::Days(days) => json!({ "AutomaticallyAfterDays": days }),
            Self::Schedule(expression, window) => {
                let mut rules = Map::new();
                rules.insert("ScheduleExpression".to_string(), json!(expression));
                if let Some(window) = window {
                    rules.insert("Duration".to_string(), json!(window));
                }
                Value::Object(rules)
            }
        }
    }
}

/// One row of the table: the forms this row's declarations name each other by, the schedule it
/// declares, and the terminal `StackStatus` the rule says its create reaches.
struct Row {
    case: &'static str,
    /// The stack, and the prefix every resource of this row is named under.
    stack: &'static str,
    secret_reference: SecretReference,
    function_reference: FunctionReference,
    rules: Rules,
    /// The terminal `StackStatus` the create must reach.
    settles_to: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "the secret by Ref and the function by its ARN, at the shortest documented interval",
        stack: "xsvc-rotation-forms-ref-and-arn",
        secret_reference: SecretReference::Ref,
        function_reference: FunctionReference::Arn,
        // The published minimum of the documented range.
        rules: Rules::Days(1),
        settles_to: CREATE_COMPLETE,
    },
    Row {
        case: "the secret by its published Id and the function by name, at the longest documented \
               interval",
        stack: "xsvc-rotation-forms-getatt-and-name",
        secret_reference: SecretReference::PublishedId,
        function_reference: FunctionReference::Name,
        // The published maximum of the documented range.
        rules: Rules::Days(1000),
        settles_to: CREATE_COMPLETE,
    },
    Row {
        case: "the secret by the name it was created under, on a rate schedule with a window",
        stack: "xsvc-rotation-forms-created-name",
        secret_reference: SecretReference::CreatedName,
        function_reference: FunctionReference::Name,
        rules: Rules::Schedule("rate(7 days)", Some("2h")),
        settles_to: CREATE_COMPLETE,
    },
    Row {
        case: "the secret by Ref, on a cron schedule naming no window",
        stack: "xsvc-rotation-forms-cron-schedule",
        secret_reference: SecretReference::Ref,
        function_reference: FunctionReference::Arn,
        rules: Rules::Schedule("cron(0 8 1 * ? *)", None),
        settles_to: CREATE_COMPLETE,
    },
    Row {
        case: "an interval one past the longest documented one, which the owning service refuses",
        stack: "xsvc-rotation-forms-past-the-maximum",
        secret_reference: SecretReference::Ref,
        function_reference: FunctionReference::Arn,
        // One past the published maximum of the documented range.
        rules: Rules::Days(1001),
        settles_to: ROLLBACK_COMPLETE,
    },
];

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

/// The name this row's secret is created under, which is also the handle a `CreatedName` row
/// names it by.
fn secret_name(row: &Row) -> String {
    format!("{}-credentials", row.stack)
}

/// The template one row deploys: the generated secret, the DB instance whose master credential
/// comes out of it, the rotation function with its execution role, the invoke grant, and the
/// schedule that binds the function to the secret — each reference written in this row's form.
fn template(row: &Row) -> String {
    let stack = row.stack;
    let instance = json!({
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

    let schedule_secret = match row.secret_reference {
        SecretReference::Ref => json!({ "Ref": "DbSecret" }),
        SecretReference::PublishedId => json!({ "Fn::GetAtt": ["DbSecret", "Id"] }),
        SecretReference::CreatedName => json!(secret_name(row)),
    };
    let granted_function = match row.function_reference {
        FunctionReference::Arn => json!({ "Fn::GetAtt": ["RotationFn", "Arn"] }),
        FunctionReference::Name => json!({ "Ref": "RotationFn" }),
    };

    let mut resources = Map::new();
    resources.insert(
        "DbSecret".to_string(),
        json!({
            "Type": "AWS::SecretsManager::Secret",
            "Properties": {
                "Name": secret_name(row),
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
    resources.insert(
        "InvokePermission".to_string(),
        json!({
            "Type": "AWS::Lambda::Permission",
            "Properties": {
                "Action": INVOKE_ACTION,
                "FunctionName": granted_function,
                "Principal": SERVICE_PRINCIPAL,
            },
        }),
    );
    resources.insert(
        "SecretRotation".to_string(),
        json!({
            "Type": "AWS::SecretsManager::RotationSchedule",
            // The grant has to be in force before the rotation this schedule configures reaches
            // the function, and the credential has to be the instance's — and a row naming the
            // secret by a literal makes no reference CloudFormation could order the two by, so
            // every row states the order it needs rather than relying on its own spelling.
            "DependsOn": ["DbSecret", "DbInstance", "InvokePermission"],
            "Properties": {
                "SecretId": schedule_secret,
                "RotationLambdaARN": { "Fn::GetAtt": ["RotationFn", "Arn"] },
                "RotationRules": row.rules.declared(),
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

/// Poll `stack` until its create has finished, in whatever status it finished in: the transitions
/// on the way are owed on the stack's own clock, so this waits for the CONDITION rather than for
/// a span.
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
    assert!(
        !reached.ends_with(IN_PROGRESS),
        "case {case:?}: expected the create to leave {IN_PROGRESS}, got {reached:?} with the \
         resource failures {:?}",
        failure_reasons(router, row.stack).await
    );
    (reached, described)
}

/// RULE: whichever published form a declaration names the resources it wires together by, the
/// create reaches `CREATE_COMPLETE`, every declared resource reports created, and the rotation
/// wiring is readable from the services that own it.
#[tokio::test]
async fn the_rotation_wiring_deploys_in_every_published_reference_form() {
    for row in ROWS.iter().filter(|row| row.settles_to == CREATE_COMPLETE) {
        let case = row.case;
        let router = build_router();
        let (reached, described) = deploy(&router, row).await;

        let (_, resources) =
            cloudformation(&router, "DescribeStackResources", &[("StackName", row.stack)]).await;
        let reported = pairs_of(&resources, "LogicalResourceId", "ResourceStatus");
        assert_eq!(
            reached, CREATE_COMPLETE,
            "case {case:?}: expected the create to settle {CREATE_COMPLETE:?}, got {reached:?} \
             with resources {reported:?} and resource failures {:?}",
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
        // that holds it — read by the ARN the stack published, whichever form the declaration
        // named the secret by.
        let (status, metadata) =
            secretsmanager(&router, "DescribeSecret", &json!({ "SecretId": &secret_arn })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeSecret, got {status} with {metadata}"
        );
        for (member, wanted) in [
            ("RotationEnabled", json!(true)),
            ("RotationLambdaARN", json!(function_arn)),
        ] {
            assert_eq!(
                metadata[member], wanted,
                "case {case:?}: expected the secret's {member} {wanted}, got {metadata}"
            );
        }
        // Every member the schedule declared reaches `RotateSecret` under its own name, so the
        // secret reports back what this row asked for — the interval, or the expression and the
        // window it named.
        let declared = row.rules.declared();
        for (member, wanted) in declared.as_object().expect("an object of rules") {
            assert_eq!(
                &metadata["RotationRules"][member], wanted,
                "case {case:?}: expected the secret's RotationRules {member} {wanted}, got \
                 {metadata}"
            );
        }

        // ...and the grant the stack declared is the function's own resource policy, so the
        // rotation this schedule configures can reach it — whichever form the grant named the
        // function by.
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

/// RULE, for the other side of the same rule: a declared value the OWNING service refuses is not
/// deployed around — the schedule's resource fails with that service's own reason and the stack
/// rolls back, so nothing reports a rotation configured on an interval Secrets Manager never
/// accepted.
#[tokio::test]
async fn an_interval_past_the_documented_range_rolls_the_deploy_back() {
    for row in ROWS.iter().filter(|row| row.settles_to == ROLLBACK_COMPLETE) {
        let case = row.case;
        let router = build_router();
        let (reached, _) = deploy(&router, row).await;

        assert_eq!(
            reached, ROLLBACK_COMPLETE,
            "case {case:?}: expected the create to settle {ROLLBACK_COMPLETE:?}, got {reached:?} \
             with the resource failures {:?}",
            failure_reasons(&router, row.stack).await
        );
        let reason = create_failure_of(&router, row.stack, "SecretRotation").await;
        let reported = reason.clone().unwrap_or_default();
        assert!(
            !reported.trim().is_empty(),
            "case {case:?}: expected a {RESOURCE_CREATE_FAILED} reason on SecretRotation, got \
             {reason:?} among the stack's reasons {:?}",
            failure_reasons(&router, row.stack).await
        );
        // The reason is the REFUSING service's own, so the right status reached for the wrong
        // cause fails here.
        let diagnostic = format!("(Service: {SECRETSMANAGER_SDK_NAME}");
        assert!(
            reported.contains(&diagnostic),
            "case {case:?}: expected the reason on SecretRotation to carry {diagnostic:?} from \
             the service that refused, got {reported:?}"
        );

        // ...and nothing the rolled-back stack created survives, so no secret of it is left
        // reporting a rotation the service refused.
        let (status, metadata) = secretsmanager(
            &router,
            "DescribeSecret",
            &json!({ "SecretId": secret_name(row) }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the rolled-back stack's secret to be gone, got {status} with \
             {metadata}"
        );
    }
}
