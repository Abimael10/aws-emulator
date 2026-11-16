// SPDX-License-Identifier: CC0-1.0

//! A secret whose rotation a CloudFormation stack provisioned really rotates when a CLIENT
//! asks for it, whatever form the stack's grant on the rotation function took.
//!
//! The rule under test: **the resource-based grant of `lambda:InvokeFunction` to the Secrets
//! Manager service principal is what lets that service rotate a secret through the function,
//! and it admits the rotation however the grant ADDRESSES the function — by the function's
//! name or by its ARN, both of which `AddPermission`'s `FunctionName` names ("Function name –
//! `my-function`. Function ARN – `arn:aws:lambda:us-west-2:123456789012:function:my-function`",
//! `smithy_query(query="operation",operation="AddPermission")`) — and however it is SCOPED to
//! that secret: unscoped, scoped by the source ARN of the secret being rotated, or scoped by
//! that secret's account (`SourceArn`, `SourceAccount`, the same operation).** So a client's
//! own `RotateSecret` on such a secret is ACCEPTED (HTTP 200 with the secret's ARN, its name
//! and the id of the version the rotation creates) rather than refused, and the rotation it
//! starts moves `AWSCURRENT` onto the version the rotation function wrote while the version it
//! replaced is demoted to `AWSPREVIOUS`.
//!
//! The rows differ along what the rule generalises over: how the `AWS::Lambda::Permission`
//! addresses the function it grants on (`Ref`, which for a function is its name, against
//! `Fn::GetAtt … Arn`), how the grant is scoped (nothing, the secret's own ARN, the secret's
//! account), and whether the deployment itself has already rotated the secret by the time the
//! client's own `RotateSecret` is made. Four crates have to agree for the rule to hold
//! (CloudFormation makes the calls, Lambda holds the function's policy and runs the function,
//! Secrets Manager reads that policy and drives the rotation, IAM answers for the function's
//! execution role), so the suite lives here rather than in any one of them.
//!
//! [`cloudformation_rotation_schedule_rotates_its_secret`](super::cloudformation_rotation_schedule_rotates_its_secret)
//! covers the other half of the same picture — how the SCHEDULE states its rules and whether
//! the deployment itself starts a rotation — with one grant form. Neither suite's rows are the
//! other's.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::execution::{
    ABANDON_GRACE, HOST_WAIT_CEILING, INIT_PHASE_LIMIT, PERMIT_WAIT,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode, opt_element, outputs_of};

/// The Region every request here is signed for, and so the Region the stack's resources live
/// in.
const REGION: &str = "us-west-2";

/// The service principal Secrets Manager invokes a rotation function as ("ensure that the
/// function policy grants access to the principal `secretsmanager.amazonaws.com`", Secrets
/// Manager User Guide, *Permissions for rotation*).
const SERVICE_PRINCIPAL: &str = "secretsmanager.amazonaws.com";

/// The action a rotation needs on the function: "you also need `lambda:InvokeFunction`
/// permissions on the rotation function" (`RotateSecret`, the Secrets Manager Smithy model).
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// How many times a rotation invokes the rotation function: "Secrets Manager calls the Lambda
/// rotation function four times, each time with a different value for the Step parameter"
/// (Secrets Manager User Guide, *How rotation works*). It is a COUNT here, used only to bound
/// how long a chain of those invocations is waited for; which steps they are, and in which
/// order, is the sibling suite's own assertion.
const ROTATION_STEP_COUNT: usize = 4;

/// The value the stack's secret is created with, which every rotation extends.
const INITIAL_VALUE: &str = "{\"username\":\"appuser\",\"password\":\"initial\"}";

/// What the rotation function puts in front of the value it stages, so the value a read
/// answers says how many rotations wrote it.
///
/// Written into the function's own source at [`PREFIX_PLACEHOLDER`] rather than restated
/// there, so the value this suite asserts on and the value the function writes cannot drift.
const ROTATED_PREFIX: &str = "rotated-";

/// Where [`ROTATED_PREFIX`] is substituted into the rotation function's source.
const PREFIX_PLACEHOLDER: &str = "@ROTATED_PREFIX@";

/// How long one invocation of the rotation function is given, as the function's own `Timeout`:
/// well past what a run needs, because what is under test is which grants admit a rotation and
/// not how fast an interpreter gets through a handler.
const FUNCTION_TIMEOUT: u64 = 30;

/// How often this suite looks again for a condition another service owes.
const POLL: Duration = Duration::from_millis(50);

/// What Lambda allows ONE invocation of the rotation function, summed over the chain of them a
/// rotation drives: its wait for capacity, the initialization of the execution environment,
/// the longest the run itself is waited for and the grace past that
/// (`service_com_amazonaws_lambda_awsgirapiservice::state::execution`).
///
/// The ceiling is DERIVED rather than picked, because a picked one is a bet on how fast the
/// host runs: every condition a loop here waits for is owed once ONE chain of invocations has
/// happened — the deployment's chain is waited out by the wait for the stack's own status, and
/// the client's by the wait for a new `AWSCURRENT` — so the ceiling is that worst case for one
/// chain, and it bounds the suite only so it cannot hang.
const INVOCATION_CHAIN_CEILING: Duration = PERMIT_WAIT
    .saturating_add(INIT_PHASE_LIMIT)
    .saturating_add(HOST_WAIT_CEILING)
    .saturating_add(ABANDON_GRACE)
    .saturating_mul(ROTATION_STEP_COUNT as u32);

/// See [`POLL`]: the ceiling in whole polls, which is what the loops count.
const POLLS: usize = INVOCATION_CHAIN_CEILING.as_millis() as usize / POLL.as_millis() as usize;

/// The assembled emulator every request of this suite is dispatched into, so the rotation
/// function's own SDK calls come back to the same Secrets Manager this suite reads.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request and what each handler reads its Region from.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one AWS Secrets Manager operation over its awsJson1_1 wire.
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
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// The same, requiring the 200 the operation answers.
async fn secretsmanager_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    request: &Value,
) -> Value {
    let (status, answered) = secretsmanager(router, operation, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {operation}, got {status} with body {answered}"
    );
    answered
}

/// Poll `stack` until CloudFormation first reports `wanted`, and answer the last status it
/// reported with the `DescribeStacks` answer that status was read from.
async fn wait_for_stack_status(
    router: &MultiServiceRouter,
    stack: &str,
    wanted: &str,
) -> (String, String) {
    let mut last = (String::new(), String::new());
    for _ in 0..POLLS {
        let (status, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "DescribeStacks {stack:?}: expected HTTP 200, got {status} with body {body}"
        );
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        last = (reported, body);
        if last.0 == wanted {
            return last;
        }
        tokio::time::sleep(POLL).await;
    }
    last
}

/// Why each resource of `stack` ended where it did, for a report about a stack that did not
/// reach the status a case expected.
async fn failure_reasons(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let (_, events) = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    elements(&events, "ResourceStatusReason")
}

/// The rotation function every row deploys: the four-step rotation AWS documents, written the
/// way the AWS rotation templates are. It reads the secret's own metadata, refuses a step whose
/// version is not staged, stages its new value on `createSecret`, proves that value is readable
/// on `testSecret` and moves `AWSCURRENT` onto it on `finishSecret` — all through this
/// service's own operations, so every version the rotation produces is one the function itself
/// wrote.
const ROTATION_FUNCTION: &str = r#"
import boto3

PREFIX = "@ROTATED_PREFIX@"


def lambda_handler(event, context):
    arn = event["SecretId"]
    token = event["ClientRequestToken"]
    step = event["Step"]
    client = boto3.client("secretsmanager")
    metadata = client.describe_secret(SecretId=arn)
    if not metadata.get("RotationEnabled"):
        raise ValueError("Secret %s is not enabled for rotation" % arn)
    versions = metadata["VersionIdsToStages"]
    if token not in versions:
        raise ValueError(
            "Secret version %s has no stage for rotation of secret %s." % (token, arn))
    if step == "createSecret":
        current = client.get_secret_value(SecretId=arn, VersionStage="AWSCURRENT")
        client.put_secret_value(
            SecretId=arn,
            ClientRequestToken=token,
            SecretString=PREFIX + current["SecretString"],
            VersionStages=["AWSPENDING"],
        )
    elif step == "setSecret":
        pass
    elif step == "testSecret":
        pending = client.get_secret_value(SecretId=arn, VersionId=token)
        if not pending["SecretString"].startswith(PREFIX):
            raise ValueError("the staged value was not the one createSecret wrote")
    elif step == "finishSecret":
        previous = None
        for version, stages in versions.items():
            if "AWSCURRENT" in stages:
                previous = version
        client.update_secret_version_stage(
            SecretId=arn,
            VersionStage="AWSCURRENT",
            MoveToVersionId=token,
            RemoveFromVersionId=previous,
        )
    else:
        raise ValueError("Invalid step parameter %s" % step)
    return {"step": step}
"#;

/// What a row's grant is SCOPED by: the property the declaration states and the value it
/// states for it, or `None` for a grant that names no source at all.
type Scope = Option<(&'static str, fn() -> Value)>;

/// One row of the table: how the stack's grant on the rotation function is declared.
struct Row {
    case: &'static str,
    /// How the `AWS::Lambda::Permission` addresses the function it grants on: `Ref`, which for
    /// a function is its NAME, or `Fn::GetAtt … Arn`, which is its ARN.
    function_name: fn() -> Value,
    /// What the grant is SCOPED by: see [`Scope`].
    scope: Scope,
    /// Whether the DEPLOYMENT itself starts a rotation, which is what the schedule's
    /// `RotateImmediatelyOnUpdate` asks for (AWS documents the default as rotating
    /// immediately); a row that holds it back leaves the client's own `RotateSecret` as the
    /// first rotation of the secret. Either way the grant is read when the schedule is
    /// configured, so both are a deployment the grant admitted.
    rotates_on_deploy: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "a grant addressing the function by its name, scoped to nothing",
        function_name: || json!({ "Ref": "RotationFunction" }),
        scope: None,
        rotates_on_deploy: true,
    },
    Row {
        case: "a grant addressing the function by its ARN, scoped to the secret being rotated",
        function_name: || json!({ "Fn::GetAtt": ["RotationFunction", "Arn"] }),
        scope: Some(("SourceArn", || json!({ "Ref": "AppSecret" }))),
        rotates_on_deploy: false,
    },
    Row {
        case: "a grant addressing the function by its name, scoped to the secret's account",
        function_name: || json!({ "Ref": "RotationFunction" }),
        scope: Some(("SourceAccount", || json!({ "Ref": "AWS::AccountId" }))),
        rotates_on_deploy: false,
    },
];

/// The policy that lets AWS Lambda assume the rotation function's execution role.
fn assume_role_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "lambda.amazonaws.com" },
            "Action": "sts:AssumeRole"
        }]
    })
}

/// The template one row deploys: the secret, the rotation function with its execution role, the
/// grant the row declares, and the schedule that binds the function to the secret.
fn template_for(row: &Row, stack: &str) -> String {
    let mut permission = json!({
        "Action": INVOKE_ACTION,
        "FunctionName": (row.function_name)(),
        "Principal": SERVICE_PRINCIPAL
    });
    if let Some((property, value)) = row.scope {
        permission[property] = value();
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "a secret a client rotates through a stack-deployed rotation function",
        "Resources": {
            "RotationRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": assume_role_policy(),
                    "Policies": [{
                        "PolicyName": "rotate-the-secret",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": "secretsmanager:*",
                                "Resource": "*"
                            }]
                        }
                    }]
                }
            },
            "RotationFunction": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Runtime": "python3.12",
                    "Handler": "index.lambda_handler",
                    "Role": { "Fn::GetAtt": ["RotationRole", "Arn"] },
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": ROTATION_FUNCTION.replace(PREFIX_PLACEHOLDER, ROTATED_PREFIX) }
                }
            },
            "InvokePermission": {
                "Type": "AWS::Lambda::Permission",
                "Properties": permission
            },
            "AppSecret": {
                "Type": "AWS::SecretsManager::Secret",
                "Properties": {
                    "Name": format!("{stack}-credentials"),
                    "SecretString": INITIAL_VALUE
                }
            },
            // The grant has to be in force before the rotation the schedule starts invokes the
            // function, which is what `DependsOn` states.
            "Schedule": {
                "Type": "AWS::SecretsManager::RotationSchedule",
                "DependsOn": "InvokePermission",
                "Properties": {
                    "SecretId": { "Ref": "AppSecret" },
                    "RotationLambdaARN": { "Fn::GetAtt": ["RotationFunction", "Arn"] },
                    "RotationRules": { "AutomaticallyAfterDays": 30 },
                    "RotateImmediatelyOnUpdate": row.rotates_on_deploy
                }
            }
        },
        "Outputs": {
            "SecretArn": { "Value": { "Ref": "AppSecret" } },
            "FunctionArn": { "Value": { "Fn::GetAtt": ["RotationFunction", "Arn"] } }
        }
    })
    .to_string()
}

/// The `AWSCURRENT` version of `secret_id`, as the pair a client reads: its id and its value.
async fn current_version(
    router: &MultiServiceRouter,
    case: &str,
    secret_id: &str,
) -> (String, String) {
    let read = secretsmanager_ok(
        router,
        case,
        "GetSecretValue",
        &json!({ "SecretId": secret_id, "VersionStage": "AWSCURRENT" }),
    )
    .await;
    (
        read["VersionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no VersionId in {read}"))
            .to_string(),
        read["SecretString"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no SecretString in {read}"))
            .to_string(),
    )
}

/// Poll `GetSecretValue` until `AWSCURRENT` names a version other than `previous`, and answer
/// what it read then. The invocations a rotation drives are that rotation's own work, so
/// nothing here sleeps a fixed span on its behalf.
async fn wait_for_a_new_current(
    router: &MultiServiceRouter,
    case: &str,
    secret_id: &str,
    previous: &str,
) -> (String, String) {
    let mut last = (String::new(), String::new());
    for _ in 0..POLLS {
        last = current_version(router, case, secret_id).await;
        if last.0 != previous {
            return last;
        }
        tokio::time::sleep(POLL).await;
    }
    last
}

/// The stages `DescribeSecret` reports for each of the secret's versions.
fn stages_of(described: &Value) -> BTreeMap<String, Vec<String>> {
    described["VersionIdsToStages"]
        .as_object()
        .map(|versions| {
            versions
                .iter()
                .map(|(version, stages)| {
                    let labels = stages
                        .as_array()
                        .map(|labels| {
                            labels
                                .iter()
                                .filter_map(|label| label.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    (version.clone(), labels)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// RULE: whatever form the stack's grant on the rotation function took — the function addressed
/// by name or by ARN, the grant unscoped or scoped to the secret or to its account — a client's
/// own `RotateSecret` on the secret the stack provisioned is ACCEPTED, and the rotation it
/// starts leaves `AWSCURRENT` on the version the rotation function wrote with the version it
/// replaced demoted to `AWSPREVIOUS`.
///
/// The rows run in sequence inside ONE test: each deploys a stack whose rotation function is a
/// real `python3.12` handler this emulator INTERPRETS, so running them as separate tests would
/// have the harness schedule them in parallel and this one suite would contend with itself for
/// the host it shares. Nothing about the rule needs them concurrent.
#[tokio::test]
async fn a_stack_declared_grant_admits_a_clients_own_rotation() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let router = build_router();
        let stack = format!("grantstack{index}");
        let template = template_for(row, &stack);
        let (status, body) = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", &stack),
                ("TemplateBody", &template),
                ("Capabilities.member.1", "CAPABILITY_IAM"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
        );

        let (reached, body) = wait_for_stack_status(&router, &stack, "CREATE_COMPLETE").await;
        assert_eq!(
            reached, "CREATE_COMPLETE",
            "case {case:?}: expected the stack to complete, got {reached:?} with {body} and \
             resource failures {:?}",
            failure_reasons(&router, &stack).await
        );
        let outputs = outputs_of(&body);
        let secret = outputs
            .get("SecretArn")
            .unwrap_or_else(|| panic!("case {case:?}: no SecretArn output in {body}"))
            .clone();
        let function = outputs
            .get("FunctionArn")
            .unwrap_or_else(|| panic!("case {case:?}: no FunctionArn output in {body}"))
            .clone();

        // The configuration the stack put in force is the secret's own, so the rotation a
        // client starts next is driven through the function the stack deployed.
        let described = secretsmanager_ok(
            &router,
            case,
            "DescribeSecret",
            &json!({ "SecretId": secret }),
        )
        .await;
        assert_eq!(
            described["RotationEnabled"].as_bool(),
            Some(true),
            "case {case:?}: expected the secret to report rotation enabled, got {described}"
        );
        assert_eq!(
            described["RotationLambdaARN"].as_str(),
            Some(function.as_str()),
            "case {case:?}: expected the declared rotation function {function:?}, got {described}"
        );
        let name = described["Name"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no Name in {described}"))
            .to_string();

        // What the DEPLOYMENT itself rotated, computed from what the row declared: a schedule
        // that starts a rotation leaves the secret on a value the function wrote by the time
        // the stack completes, and one that holds it back leaves the declared value. Pinned
        // before the client's own rotation, so the value the next reads are compared against is
        // the one in force rather than whatever a deployment rotation still in flight left.
        let mut expected_before = INITIAL_VALUE.to_string();
        if row.rotates_on_deploy {
            expected_before = format!("{ROTATED_PREFIX}{expected_before}");
        }
        let (before_id, before_value) = current_version(&router, case, &secret).await;
        assert_eq!(
            before_value, expected_before,
            "case {case:?}: expected the value in force once the stack completed to be \
             {expected_before:?}, got {before_value:?}"
        );

        // The call the rule is about: a client's own `RotateSecret`, ACCEPTED rather than
        // refused, answering the secret's ARN, its name and the id of the version the rotation
        // creates (`RotateSecretResponse`, the Secrets Manager Smithy model).
        let (status, started) =
            secretsmanager(&router, "RotateSecret", &json!({ "SecretId": secret })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the rotation to be accepted with HTTP 200, got {status} \
             with body {started}"
        );
        assert_eq!(
            started["ARN"].as_str(),
            Some(secret.as_str()),
            "case {case:?}: expected the secret's ARN {secret:?} in the answer, got {started}"
        );
        assert_eq!(
            started["Name"].as_str(),
            Some(name.as_str()),
            "case {case:?}: expected the secret's name {name:?} in the answer, got {started}"
        );
        let staked = started["VersionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no VersionId in {started}"))
            .to_string();
        assert_ne!(
            staked, before_id,
            "case {case:?}: expected the rotation to stake a new version, got the one in use \
             {before_id:?}"
        );

        // The rotation really ran: `AWSCURRENT` moved onto the version the function created,
        // and its value is the one that function wrote.
        let (after_id, after_value) =
            wait_for_a_new_current(&router, case, &secret, &before_id).await;
        assert_eq!(
            after_id, staked,
            "case {case:?}: expected AWSCURRENT on the version the rotation staked {staked:?}, \
             got {after_id:?}"
        );
        assert_eq!(
            after_value,
            format!("{ROTATED_PREFIX}{before_value}"),
            "case {case:?}: expected the value the rotation function wrote, got {after_value:?}"
        );

        let described = secretsmanager_ok(
            &router,
            case,
            "DescribeSecret",
            &json!({ "SecretId": secret }),
        )
        .await;
        let stages = stages_of(&described);
        assert!(
            stages
                .get(&after_id)
                .is_some_and(|labels| labels.iter().any(|label| label == "AWSCURRENT")),
            "case {case:?}: expected the rotated version {after_id:?} to carry AWSCURRENT, got \
             {stages:?}"
        );
        assert!(
            stages
                .get(&before_id)
                .is_some_and(|labels| labels.iter().any(|label| label == "AWSPREVIOUS")),
            "case {case:?}: expected the replaced version {before_id:?} to carry AWSPREVIOUS, \
             got {stages:?}"
        );
        let previous = secretsmanager_ok(
            &router,
            case,
            "GetSecretValue",
            &json!({ "SecretId": secret, "VersionStage": "AWSPREVIOUS" }),
        )
        .await;
        assert_eq!(
            previous["SecretString"].as_str(),
            Some(before_value.as_str()),
            "case {case:?}: expected AWSPREVIOUS to answer the replaced value, got {previous}"
        );
    }
}
