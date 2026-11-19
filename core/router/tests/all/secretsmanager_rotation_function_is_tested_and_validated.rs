// SPDX-License-Identifier: CC0-1.0

//! What `RotateSecret` does with the rotation FUNCTION the call names: a call that is not to
//! rotate now still reaches that function once, and a value that names no function is refused
//! with the form such a value has to have.
//!
//! The two rules under test:
//!
//! * **A deferred rotation TESTS its function.** "If you set RotateImmediately to false,
//!   Secrets Manager tests the rotation configuration by running the testSecret step of the
//!   Lambda rotation function. This test creates an AWSPENDING version of the secret and then
//!   removes it" (`RotateSecretRequest$RotateImmediately`, Secrets Manager's Smithy model) — so
//!   such a call invokes the configured function exactly once, with `Step` = `testSecret`, off
//!   the response path, and leaves the secret where it was: rotation enabled with a next due
//!   instant, the staging map back to the current version alone, no `LastRotatedDate` and the
//!   value the secret already served. That one invocation is how a client that only SCHEDULES a
//!   rotation learns at configuration time that its function can be invoked and runs, instead
//!   of finding out at the first unattended rotation.
//! * **A `RotationLambdaARN` that names no Lambda function is refused with the FORM it must
//!   have.** `RotationLambdaARN` is modelled with a length of 0..2048 and no pattern, so the
//!   service judges the value; a rotation is driven by INVOKING what it names, so a value that
//!   is not a Lambda function's ARN is refused 400 `InvalidRequestException` stating the ARN
//!   form, whose only variable part is the Region the call was made in. The sentence for a
//!   secret that has NO rotation function is reserved for the call that names none and whose
//!   secret stores none: both conditions travel under the same error code, so the sentence is
//!   the only thing that tells a caller to fix the ARN it passed rather than attach a function.
//!
//! Both rules are asserted over TABLES: the first over how the deferred call states its
//! schedule, whether it names a `ClientRequestToken` and how it names the secret; the second
//! over the shapes of value that name no function (no ARN at all, another service's ARN,
//! another Lambda resource's ARN, a function ARN with no name) and over two Regions, since the
//! Region is what the sentence interpolates.
//!
//! Five crates have to agree for the first rule to hold — CloudFormation deploys the rotation
//! function, IAM answers for its execution role and for the grant that lets Secrets Manager
//! invoke it, Lambda runs it, Secrets Manager performs the test — so the suite lives here.
//! Every wait below looks for the STATE it waits on, and its ceiling is there only so the
//! suite cannot hang.
//!
//! This suite also OWNS the harness its sibling
//! `secretsmanager_rotation_resumes_a_cancelled_candidate` drives the same estate through —
//! the assembled router, the two wires, the stack that deploys a rotation function and the
//! readers of what that function records — so there is one copy of it rather than two.

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

/// The Region the deferred-rotation table signs for, and so the Region its stack and its
/// secrets live in.
pub(crate) const REGION: &str = "us-west-2";

/// The service principal Secrets Manager invokes a rotation function as ("ensure that the
/// function policy grants access to the principal `secretsmanager.amazonaws.com`", Secrets
/// Manager User Guide, *Permissions for rotation*).
const SERVICE_PRINCIPAL: &str = "secretsmanager.amazonaws.com";

/// The step a deferred rotation tests the configuration with, and the three it does NOT run:
/// "Secrets Manager calls the Lambda rotation function four times, each time with a different
/// value for the Step parameter" is what a rotation does, while the test runs this one alone.
pub(crate) const TEST_STEP: &str = "testSecret";
pub(crate) const ROTATION_STEPS: [&str; 4] =
    ["createSecret", "setSecret", TEST_STEP, "finishSecret"];

/// The staging label the version a secret currently serves carries.
pub(crate) const CURRENT_STAGE: &str = "AWSCURRENT";

/// The tag the rotation function records the steps it was invoked with under, in order: the
/// suite's window onto the invocations, read back through `DescribeSecret`.
const STEPS_TAG: &str = "RotationSteps";

/// The value every secret below is created with, which a deferred rotation must leave alone.
const INITIAL_VALUE: &str = "the-credential-before-any-rotation";

/// How long one invocation of the rotation function is given, as the function's own `Timeout`:
/// well past what a run needs, because what is under test is the rotation protocol and not how
/// fast an interpreter gets through a handler.
const FUNCTION_TIMEOUT: u64 = 30;

/// How long this suite waits for a condition another service owes, and how often it looks.
///
/// The ceiling is DERIVED rather than picked: every condition here is owed only once an
/// invocation of the rotation function has happened, and Lambda may spend its own published
/// bounds on one — the wait for capacity to run in, the initialization of the execution
/// environment, the longest the run itself is waited for and the grace past that
/// (`service_com_amazonaws_lambda_awsgirapiservice::state::execution`). The ceiling is that
/// worst case for a whole rotation's worth of invocations, which holds on a host busy building
/// the rest of the workspace rather than only on an idle one.
pub(crate) const POLL: Duration = Duration::from_millis(50);
const INVOCATION_CHAIN_CEILING: Duration = PERMIT_WAIT
    .saturating_add(INIT_PHASE_LIMIT)
    .saturating_add(HOST_WAIT_CEILING)
    .saturating_add(ABANDON_GRACE)
    .saturating_mul(ROTATION_STEPS.len() as u32);
/// See [`POLL`]: the ceiling in whole polls, which is what the loops count.
pub(crate) const POLLS: usize =
    INVOCATION_CHAIN_CEILING.as_millis() as usize / POLL.as_millis() as usize;

/// The emulator as it is ASSEMBLED, on one engine (`aws_emulator_router::build_router`): the
/// rotation function's own SDK calls are dispatched back into this same router, so the secret
/// it reads is the secret this suite wrote.
pub(crate) fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in `region`, which is
/// what routes the request and what each handler reads its Region from.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{region}/{service}/\
         aws4_request, SignedHeaders=host, \
         Signature=0000000000000000000000000000000000000000000000000000000000000000"
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
        .header("authorization", authorization("cloudformation", REGION))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one Secrets Manager operation over its awsJson1_1 wire, in `region`.
pub(crate) async fn secretsmanager(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("secretsmanager.{region}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("secretsmanager.{operation}"))
        .header("authorization", authorization("secretsmanager", region))
        .body(Bytes::from(request.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// The same, requiring the 200 the operation answers.
pub(crate) async fn secretsmanager_ok(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    operation: &str,
    request: &Value,
) -> Value {
    let (status, answered) = secretsmanager(router, region, operation, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {operation}, got {status} with body {answered}"
    );
    answered
}

/// The same, requiring a refusal, as its status, wire error code and message.
pub(crate) async fn secretsmanager_err(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    operation: &str,
    request: &Value,
) -> (u16, String, String) {
    let (status, answered) = secretsmanager(router, region, operation, request).await;
    assert_ne!(
        status, 200,
        "case {case:?}: expected {operation} to be refused, got 200 with {answered}"
    );
    let code = answered["__type"]
        .as_str()
        .and_then(|raw| raw.split('#').next_back())
        .unwrap_or_default()
        .to_string();
    let message = answered["message"]
        .as_str()
        .or_else(|| answered["Message"].as_str())
        .unwrap_or_default()
        .to_string();
    (status, code, message)
}

/// A `ClientRequestToken` (and so a version id) of 32 characters, unique per `seed`:
/// `ClientRequestTokenType` is modelled with a length of 32..64.
pub(crate) fn token(seed: &str) -> String {
    let mut id = format!("tok-{seed}-");
    while id.len() < 32 {
        id.push('0');
    }
    id.chars().take(32).collect()
}

/// The rotation function every row of the first table is tested against: it RECORDS the step it
/// was invoked with before doing anything else, which is what says the function ran at all —
/// the recorded behaviour this rule was taken from reads the same fact out of the function's own
/// log. It writes nothing else, so a test invocation cannot leave a version behind by way of the
/// function either.
const RECORDING_FUNCTION: &str = r#"
import boto3

STEPS_TAG = "RotationSteps"


def lambda_handler(event, context):
    arn = event["SecretId"]
    step = event["Step"]
    client = boto3.client("secretsmanager")
    metadata = client.describe_secret(SecretId=arn)
    recorded = ""
    for tag in metadata.get("Tags", []):
        if tag["Key"] == STEPS_TAG:
            recorded = tag["Value"]
    recorded = recorded + "," + step if recorded else step
    client.tag_resource(SecretId=arn, Tags=[{"Key": STEPS_TAG, "Value": recorded}])
    return {"step": step}
"#;

/// The template deploying `source` as a rotation function: its execution role, the function
/// itself, and the grant that lets Secrets Manager invoke it.
fn function_template(source: &str) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "a rotation function a secret's rotation is driven through",
        "Resources": {
            "RotationRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": { "Service": "lambda.amazonaws.com" },
                            "Action": "sts:AssumeRole"
                        }]
                    },
                    "Policies": [{
                        "PolicyName": "read-and-tag-the-secret",
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
                    "Code": { "ZipFile": source }
                }
            },
            "InvokePermission": {
                "Type": "AWS::Lambda::Permission",
                "Properties": {
                    "Action": "lambda:InvokeFunction",
                    "FunctionName": { "Fn::GetAtt": ["RotationFunction", "Arn"] },
                    "Principal": SERVICE_PRINCIPAL
                }
            }
        },
        "Outputs": {
            "FunctionArn": { "Value": { "Fn::GetAtt": ["RotationFunction", "Arn"] } }
        }
    })
    .to_string()
}

/// Poll `stack` until CloudFormation first reports `wanted`, and answer the last status it
/// reported with the body that status was read from.
async fn wait_for_stack_status(
    router: &MultiServiceRouter,
    stack: &str,
    wanted: &str,
) -> (String, String) {
    let mut last = (String::new(), String::new());
    for _ in 0..POLLS {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
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

/// Deploy `source` as a rotation function Secrets Manager may invoke, and answer its ARN.
pub(crate) async fn deployed_function(
    router: &MultiServiceRouter,
    stack: &str,
    source: &str,
) -> String {
    let template = function_template(source);
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", &template),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    let (reached, body) = wait_for_stack_status(router, stack, "CREATE_COMPLETE").await;
    let reasons = {
        let (_, events) =
            cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
        elements(&events, "ResourceStatusReason")
    };
    assert_eq!(
        reached, "CREATE_COMPLETE",
        "setup: expected the rotation function's stack to complete, got {reached:?} with {body} \
         and resource failures {reasons:?}"
    );
    outputs_of(&body)
        .get("FunctionArn")
        .unwrap_or_else(|| panic!("setup: no FunctionArn output in {body}"))
        .clone()
}

/// The stages `DescribeSecret` reports for each of the secret's versions.
pub(crate) fn stages_of(described: &Value) -> BTreeMap<String, Vec<String>> {
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

/// The steps the rotation function recorded for this secret, in the order it was invoked with
/// them.
pub(crate) fn recorded_steps(described: &Value) -> Vec<String> {
    described["Tags"]
        .as_array()
        .and_then(|tags| {
            tags.iter()
                .find(|tag| tag["Key"].as_str() == Some(STEPS_TAG))
                .and_then(|tag| tag["Value"].as_str())
        })
        .map(|recorded| recorded.split(',').map(str::to_string).collect())
        .unwrap_or_default()
}

/// What `DescribeSecret` reports for `secret_id`.
pub(crate) async fn described(
    router: &MultiServiceRouter,
    case: &str,
    secret_id: &str,
) -> Value {
    secretsmanager_ok(
        router,
        case,
        REGION,
        "DescribeSecret",
        &json!({ "SecretId": secret_id }),
    )
    .await
}

/// Poll `DescribeSecret` until the rotation function has recorded at least `wanted` steps for
/// `secret_id`, and answer the last answer read. Nothing here sleeps a span on the invocation's
/// behalf: the invocation is the service's own work, and this looks for the record of it.
pub(crate) async fn wait_for_steps(
    router: &MultiServiceRouter,
    case: &str,
    secret_id: &str,
    wanted: usize,
) -> Value {
    let mut last = Value::Null;
    for _ in 0..POLLS {
        last = described(router, case, secret_id).await;
        if recorded_steps(&last).len() >= wanted {
            return last;
        }
        tokio::time::sleep(POLL).await;
    }
    last
}

/// One row of the deferred-rotation table: how the call states its schedule, whether it names a
/// token of its own, and how it names the secret.
struct DeferredRow {
    case: &'static str,
    /// The `RotationRules` the call states, which `DescribeSecret` must then report unchanged.
    rules: fn() -> Value,
    /// Whether the call names a `ClientRequestToken` of its own.
    names_token: bool,
    /// Whether the call addresses the secret by its ARN rather than by its name.
    by_arn: bool,
}

const DEFERRED_ROWS: &[DeferredRow] = &[
    DeferredRow {
        case: "a day-interval schedule, with a token of the caller's own, named by name",
        rules: || json!({ "AutomaticallyAfterDays": 30 }),
        names_token: true,
        by_arn: false,
    },
    DeferredRow {
        case: "a rate() schedule with a rotation window, no token, named by ARN",
        rules: || json!({ "ScheduleExpression": "rate(12 hours)", "Duration": "2h" }),
        names_token: false,
        by_arn: true,
    },
    DeferredRow {
        case: "a cron() schedule, with a token of the caller's own, named by ARN",
        rules: || json!({ "ScheduleExpression": "cron(0 8 1 * ? *)" }),
        names_token: true,
        by_arn: false,
    },
];

/// RULE: a `RotateSecret` that asks NOT to rotate now still tests the rotation function it
/// configures, by invoking it once with the `testSecret` step — and leaves the secret where it
/// was.
///
/// The rows run on one function, deployed once: what they vary is the CALL, and the function's
/// own identity is no part of it.
#[tokio::test]
async fn a_deferred_rotation_tests_its_function_with_the_test_step() {
    let router = build_router();
    let function = deployed_function(&router, "deferredrotationstack", RECORDING_FUNCTION).await;

    for (index, row) in DEFERRED_ROWS.iter().enumerate() {
        let case = row.case;
        let name = format!("deferred-credentials-{index}");
        let created = secretsmanager_ok(
            &router,
            case,
            REGION,
            "CreateSecret",
            &json!({
                "Name": name,
                "ClientRequestToken": token(&format!("init{index}")),
                "SecretString": INITIAL_VALUE,
            }),
        )
        .await;
        let arn = created["ARN"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no ARN in {created}"))
            .to_string();
        let current = created["VersionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no VersionId in {created}"))
            .to_string();
        let secret_id = if row.by_arn { arn.clone() } else { name.clone() };

        let mut request = json!({
            "SecretId": secret_id,
            "RotationLambdaARN": function,
            "RotationRules": (row.rules)(),
            "RotateImmediately": false,
        });
        let named = token(&format!("deferred{index}"));
        if row.names_token {
            request["ClientRequestToken"] = json!(named);
        }
        let answered = secretsmanager_ok(&router, case, REGION, "RotateSecret", &request).await;
        if row.names_token {
            assert_eq!(
                answered["VersionId"].as_str(),
                Some(named.as_str()),
                "case {case:?}: expected the token the call named back, got {answered}"
            );
        }

        // The ONE invocation the test performs, waited for by the record the function writes.
        let described = wait_for_steps(&router, case, &name, 1).await;
        assert_eq!(
            recorded_steps(&described),
            vec![TEST_STEP.to_string()],
            "case {case:?}: expected the deferred rotation to invoke the function once with \
             {TEST_STEP:?}, got {described}"
        );

        // ... and the secret is where it was: the configuration in force, no rotation
        // performed, and the test's candidate version gone from the staging map.
        assert_eq!(
            described["RotationEnabled"].as_bool(),
            Some(true),
            "case {case:?}: expected rotation enabled by the deferred call, got {described}"
        );
        assert_eq!(
            described["RotationLambdaARN"].as_str(),
            Some(function.as_str()),
            "case {case:?}: expected the configured rotation function {function:?}, got \
             {described}"
        );
        assert_eq!(
            described["RotationRules"],
            (row.rules)(),
            "case {case:?}: expected the stated rotation rules, got {described}"
        );
        assert!(
            described.get("NextRotationDate").is_some(),
            "case {case:?}: expected the stated schedule to make a rotation due, got {described}"
        );
        assert!(
            described.get("LastRotatedDate").is_none(),
            "case {case:?}: expected a call that rotated nothing to leave LastRotatedDate \
             unset, got {described}"
        );
        assert_eq!(
            stages_of(&described),
            BTreeMap::from([(current.clone(), vec![CURRENT_STAGE.to_string()])]),
            "case {case:?}: expected the staging map to hold the current version alone, got \
             {described}"
        );
        let read = secretsmanager_ok(
            &router,
            case,
            REGION,
            "GetSecretValue",
            &json!({ "SecretId": secret_id, "VersionStage": CURRENT_STAGE }),
        )
        .await;
        assert_eq!(
            read["SecretString"].as_str(),
            Some(INITIAL_VALUE),
            "case {case:?}: expected the value the secret already served, got {read}"
        );
    }
}

/// The error code both refusals below travel under: "a parameter value is not valid for the
/// current state of the resource" (`InvalidRequestException`, Secrets Manager's Smithy model).
const INVALID_REQUEST: &str = "InvalidRequestException";

/// The sentence reserved for a call that names NO rotation function and whose secret stores
/// none — the documented cause "you tried to enable rotation on a secret that doesn't already
/// have a Lambda function ARN configured and you didn't include such an ARN as a parameter in
/// this call".
const NO_ROTATION_FUNCTION: &str = "No Lambda rotation function ARN is associated with this \
                                    secret.";

/// The sentence for a `RotationLambdaARN` that names no Lambda function: the ARN form the value
/// has to have, with the Region taken from the request and `<account>` and `<functionName>`
/// stated as the placeholders of that form.
fn invalid_rotation_arn(region: &str) -> String {
    format!(
        "Invalid rotation Lambda ARN. Ensure the ARN matches format \
         arn:aws:lambda:{region}:<account>:function:<functionName>."
    )
}

/// The values that name no Lambda function, as a caller sends them.
///
/// One per shape the judgement turns on: a value that is no ARN at all, a bare function name,
/// another service's resource, another of Lambda's own resource types, and a function ARN whose
/// function name is empty.
const NOT_A_FUNCTION: &[(&str, &str)] = &[
    ("a value that is no ARN at all", "not-a-lambda-arn-at-all"),
    ("a bare function name", "RotateMySecret"),
    (
        "another service's resource",
        "arn:aws:sqs:us-west-2:000000000000:rotation-queue",
    ),
    (
        "another of Lambda's own resource types",
        "arn:aws:lambda:us-west-2:000000000000:layer:rotation:1",
    ),
    (
        "a function ARN naming no function",
        "arn:aws:lambda:us-west-2:000000000000:function:",
    ),
];

/// RULE: a `RotationLambdaARN` that names no Lambda function is refused with the form such a
/// value has to have, and the sentence for a secret that HAS no rotation function is reserved
/// for the call that names none.
///
/// The rows run over the shapes of value that name no function and over two Regions, because
/// the Region is the one part of the sentence the request decides.
#[tokio::test]
async fn a_rotation_arn_that_names_no_function_states_the_form_it_must_have() {
    let router = build_router();
    for region in ["us-west-2", "eu-central-1"] {
        // The secret namespace is per Region, so each Region gets the secret its calls name.
        let name = format!("arnform-credentials-{region}");
        secretsmanager_ok(
            &router,
            "setup",
            region,
            "CreateSecret",
            &json!({
                "Name": name,
                "ClientRequestToken": token(region),
                "SecretString": INITIAL_VALUE,
            }),
        )
        .await;

        for (shape, value) in NOT_A_FUNCTION {
            let case = format!("{shape} in {region}");
            let (status, code, message) = secretsmanager_err(
                &router,
                &case,
                region,
                "RotateSecret",
                &json!({ "SecretId": name, "RotationLambdaARN": value }),
            )
            .await;
            assert_eq!(
                (status, code.as_str()),
                (400, INVALID_REQUEST),
                "case {case:?}: expected 400 {INVALID_REQUEST}, got {status} {code:?} with \
                 {message:?}"
            );
            let expected = invalid_rotation_arn(region);
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
        }

        // The reserved sentence, for the call that names no rotation function at all.
        let case = format!("a call naming no rotation function in {region}");
        let (status, code, message) = secretsmanager_err(
            &router,
            &case,
            region,
            "RotateSecret",
            &json!({ "SecretId": name }),
        )
        .await;
        assert_eq!(
            (status, code.as_str()),
            (400, INVALID_REQUEST),
            "case {case:?}: expected 400 {INVALID_REQUEST}, got {status} {code:?} with \
             {message:?}"
        );
        assert_eq!(
            message, NO_ROTATION_FUNCTION,
            "case {case:?}: expected {NO_ROTATION_FUNCTION:?}, got {message:?}"
        );

        // Every refusal above left the secret as it was: no rotation configured by a call
        // that was not served.
        let described = secretsmanager_ok(
            &router,
            &case,
            region,
            "DescribeSecret",
            &json!({ "SecretId": name }),
        )
        .await;
        assert_eq!(
            described.get("RotationEnabled"),
            None,
            "case {case:?}: expected the refused calls to configure no rotation, got {described}"
        );
        assert_eq!(
            described.get("RotationLambdaARN"),
            None,
            "case {case:?}: expected no rotation function stored by a refused call, got \
             {described}"
        );
    }
}
