// SPDX-License-Identifier: CC0-1.0

//! The re-drive of a rotation that ended unfinished is decided by its `ClientRequestToken`
//! ALONE, and carries no configuration of its own: the rotation goes through the function the
//! secret already stores, as that function stands at the moment of the call.
//!
//! The rule under test: **on a secret whose outstanding rotation has ENDED — its function ran a
//! step and did not complete it, so nothing is driving that rotation any more — `RotateSecret`
//! decides on the request's `ClientRequestToken`: the token naming the outstanding candidate is
//! that rotation driven again ("you only need to specify this value if you implement your own
//! retry logic and you want to ensure that Secrets Manager doesn't attempt to create a secret
//! version twice. [...] Secrets Manager uses this value to prevent the accidental creation of
//! duplicate versions if there are failures and retries during a rotation",
//! `RotateSecretRequest$ClientRequestToken`, `@idempotencyToken`, Secrets Manager's Smithy
//! model), while EVERY other token asks for a rotation beside the outstanding one and is refused
//! 400 `InvalidRequestException` with the fixed sentence "A previous rotation isn't complete.
//! That rotation will be reattempted." — whether that token names no version of the secret at
//! all or names another version it already holds. A re-drive needs to carry nothing else:
//! "if you don't include the configuration parameters, the operation starts a rotation with the
//! values already stored in the secret" (`RotateSecret`, same model), so a call carrying only the
//! secret and that token runs the stored rotation function, with the stored schedule, and leaves
//! both as they were.**
//!
//! That is the recovery AWS documents for a rotation function that failed part-way: repair the
//! function and retry the call with the token the rotation already holds. The client repairs it
//! IN PLACE here — `UpdateFunctionConfiguration` on the function's own environment, which is how
//! the behaviour this rule was taken from was gated — so the re-driving call names no
//! `RotationLambdaARN` and no `RotationRules`, and what drives the resumed rotation can only be
//! what the secret stores.
//!
//! The rows span what the rule generalises over: WHICH step the earlier rotation stopped at, so
//! the candidate the re-drive takes up is in turn one staged but not yet in force in the service
//! that owns the credential and one that passed its test and was never promoted. Each row checks
//! both shapes of token that are NOT that candidate's — one no version of the secret is stored
//! under, and the id of the version `AWSCURRENT` is on — and that each is refused without
//! staking or minting anything.
//!
//! What the siblings own and this suite does not repeat:
//! [`crate::secretsmanager_rotation_is_re_driven_by_its_own_token`] pins the re-drive made by a
//! call that RE-POINTS the rotation at another function and names the secret by ARN;
//! [`crate::secretsmanager_rotation_stops_at_the_step_its_function_raises_in`] pins what a
//! rotation that stops leaves behind, step by step; and
//! [`crate::secretsmanager_rotation_resumes_a_cancelled_candidate`] pins the other way out of an
//! unfinished rotation. Four crates have to agree for this rule to hold — CloudFormation deploys
//! the rotation function and the role it runs as, IAM answers for that role and for the grant
//! Secrets Manager invokes it under, Lambda runs each invocation and takes the repair, Secrets
//! Manager drives the protocol — so the suite lives here and reuses the harness
//! [`crate::secretsmanager_rotation_function_is_tested_and_validated`] owns. Every wait looks for
//! the STATE it waits on, and its ceiling is there only so the suite cannot hang.

use std::collections::BTreeMap;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode, opt_element, outputs_of};
use crate::secretsmanager_rotation_function_is_tested_and_validated::{
    build_router, described, recorded_steps, secretsmanager, secretsmanager_err as refused_in,
    secretsmanager_ok as served_in, stages_of, token, wait_for_steps, CURRENT_STAGE, POLL, POLLS,
    REGION, ROTATION_STEPS,
};

/// The staging label an outstanding candidate carries, and the one the version a promotion
/// replaced carries afterwards.
const PENDING_STAGE: &str = "AWSPENDING";
const PREVIOUS_STAGE: &str = "AWSPREVIOUS";

/// The value every row's secret is created with, and what its rotation function appends to it
/// when it stages the candidate.
const INITIAL_VALUE: &str = "the-credential-before-any-rotation";
const ROTATED_SUFFIX: &str = "-rotated";

/// The sentence a call asking for a rotation beside the outstanding one is refused with. Fixed
/// text: it names no version, because what it reports is that a rotation of this secret has not
/// finished.
const ROTATION_IN_PROGRESS: &str =
    "A previous rotation isn't complete. That rotation will be reattempted.";

/// The schedule the FIRST call of every row configures, in days, and which the re-drive — naming
/// no rules of its own — must leave in force.
const SCHEDULE_DAYS: u64 = 30;

/// The service principal Secrets Manager invokes a rotation function as ("ensure that the
/// function policy grants access to the principal `secretsmanager.amazonaws.com`", Secrets
/// Manager User Guide, *Permissions for rotation*).
const SERVICE_PRINCIPAL: &str = "secretsmanager.amazonaws.com";

/// How long one invocation of the rotation function is given, as the function's own `Timeout`:
/// well past what a run needs, because what is under test is the rotation protocol and not how
/// fast an interpreter gets through a handler.
const FUNCTION_TIMEOUT: u64 = 30;

/// The function every row rotates through, and the environment variable it reads the step it
/// cannot complete from: the client's repair is a write of exactly that variable.
const FUNCTION_NAME: &str = "redrivestoredrotator";
const FAIL_AT_VARIABLE: &str = "FAIL_AT";

/// The `State` a function takes invocations in — "the function is ready to be invoked"
/// (`FunctionConfiguration$State`) — and the `LastUpdateStatus` of one whose update is still
/// going on.
const STATE_ACTIVE: &str = "Active";
const UPDATE_IN_PROGRESS: &str = "InProgress";

/// The rotation function every row is driven through, written the way the AWS rotation templates
/// are: it RECORDS the step it was invoked with before anything else — which is how a row waits
/// for a rotation that cannot finish — and then does that step's own work through Secrets
/// Manager's own operations, leaving a candidate an earlier attempt already staged alone.
///
/// It raises on the step the environment names, read PER INVOCATION, so the step it cannot
/// complete is a property of the function as it stands when Secrets Manager invokes it — which is
/// what the client repairs between the rotation that stops and the one that re-drives it.
const ROTATION_FUNCTION: &str = r#"
import os

import boto3
from botocore.exceptions import ClientError

STEPS_TAG = "RotationSteps"
SUFFIX = "-rotated"


def record(client, arn, step):
    metadata = client.describe_secret(SecretId=arn)
    recorded = ""
    for tag in metadata.get("Tags", []):
        if tag["Key"] == STEPS_TAG:
            recorded = tag["Value"]
    recorded = recorded + "," + step if recorded else step
    client.tag_resource(SecretId=arn, Tags=[{"Key": STEPS_TAG, "Value": recorded}])
    return metadata


def staged(client, arn, token):
    try:
        return client.get_secret_value(SecretId=arn, VersionId=token)
    except ClientError:
        return None


def lambda_handler(event, context):
    arn = event["SecretId"]
    token = event["ClientRequestToken"]
    step = event["Step"]
    client = boto3.client("secretsmanager")
    metadata = record(client, arn, step)
    if step == os.environ.get("FAIL_AT", ""):
        raise RuntimeError("this rotation function cannot complete the %s step" % step)
    versions = metadata.get("VersionIdsToStages", {})
    if step == "createSecret":
        if staged(client, arn, token) is None:
            current = client.get_secret_value(SecretId=arn, VersionStage="AWSCURRENT")
            client.put_secret_value(
                SecretId=arn,
                ClientRequestToken=token,
                SecretString=current["SecretString"] + SUFFIX,
                VersionStages=["AWSPENDING"],
            )
    elif step == "setSecret":
        pass
    elif step == "testSecret":
        if staged(client, arn, token) is None:
            raise ValueError("the candidate %s holds no value to test" % token)
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

/// One row: the step the earlier rotation's function could not complete, which decides what state
/// its candidate is left in, and the names this row's secret and tokens are built from.
struct Row {
    case: &'static str,
    fail_at: &'static str,
    id: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "a candidate staged but not yet in force, left by a function that raised in setSecret",
        fail_at: "setSecret",
        id: "storedunset",
    },
    Row {
        case: "a tested candidate never promoted, left by a function that raised in finishSecret",
        fail_at: "finishSecret",
        id: "storedunpromoted",
    },
];

impl Row {
    /// The steps this row's first rotation gets THROUGH, the raising one included: the protocol
    /// stops at the first step that did not complete.
    fn stopped_after(&self) -> Vec<String> {
        let reached = ROTATION_STEPS
            .iter()
            .position(|step| *step == self.fail_at)
            .map_or(ROTATION_STEPS.len(), |at| at + 1);
        ROTATION_STEPS[..reached]
            .iter()
            .map(|step| (*step).to_string())
            .collect()
    }

    /// The steps recorded once the re-driven rotation has run as well: the ones its predecessor
    /// got through, then the whole protocol again.
    fn after_the_re_drive(&self) -> Vec<String> {
        let mut steps = self.stopped_after();
        steps.extend(ROTATION_STEPS.iter().map(|step| (*step).to_string()));
        steps
    }
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request and what each handler reads its Region from.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
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
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one Lambda operation over its restJson1 wire, as the status it answered and its body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = match &body {
        Some(body) => Bytes::from(body.to_string()),
        None => Bytes::new(),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(payload)
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The template deploying the one rotation function these rows share: its execution role, the
/// function itself — named, so the client can repair it — and the grant that lets Secrets Manager
/// invoke it.
fn function_template() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "the rotation function a re-driven rotation is driven through",
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
                        "PolicyName": "rotate-and-tag-the-secret",
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
                    "FunctionName": FUNCTION_NAME,
                    "Runtime": "python3.12",
                    "Handler": "index.lambda_handler",
                    "Role": { "Fn::GetAtt": ["RotationRole", "Arn"] },
                    "Timeout": FUNCTION_TIMEOUT,
                    "Environment": { "Variables": { FAIL_AT_VARIABLE: "" } },
                    "Code": { "ZipFile": ROTATION_FUNCTION }
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

/// Deploy that template and answer the function's ARN, which is the value the first call of every
/// row configures as the secret's `RotationLambdaARN`.
async fn deployed_function(router: &MultiServiceRouter, stack: &str) -> String {
    let template = function_template();
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
    let mut last = (String::new(), String::new());
    for _ in 0..POLLS {
        let (status, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "setup: expected HTTP 200 from DescribeStacks, got {status} with body {body}"
        );
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        last = (reported, body);
        if last.0 == "CREATE_COMPLETE" {
            break;
        }
        tokio::time::sleep(POLL).await;
    }
    let reasons = {
        let (_, events) = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
        elements(&events, "ResourceStatusReason")
    };
    assert_eq!(
        last.0, "CREATE_COMPLETE",
        "setup: expected the rotation function's stack to complete, got {:?} with {} and resource \
         failures {reasons:?}",
        last.0, last.1
    );
    outputs_of(&last.1)
        .get("FunctionArn")
        .unwrap_or_else(|| panic!("setup: no FunctionArn output in {}", last.1))
        .clone()
}

/// Poll `GetFunctionConfiguration` until Lambda first reports the shared function invocable —
/// `Active` with no update in flight — so nothing here sleeps a span on that service's behalf.
async fn wait_until_invocable(router: &MultiServiceRouter, case: &str) {
    let mut last = Value::Null;
    for _ in 0..POLLS {
        let (_status, read) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION_NAME}/configuration"),
            None,
        )
        .await;
        let active = read.get("State").and_then(Value::as_str) == Some(STATE_ACTIVE);
        let settled = read
            .get("LastUpdateStatus")
            .and_then(Value::as_str)
            .is_none_or(|status| status != UPDATE_IN_PROGRESS);
        last = read;
        if active && settled {
            return;
        }
        tokio::time::sleep(POLL).await;
    }
    panic!("case {case:?}: Lambda never reported {FUNCTION_NAME} invocable, last read {last}");
}

/// Write the step the shared rotation function cannot complete into its own environment, and wait
/// until Lambda reports the function invocable again: the client's repair, and the client's break,
/// made the one way a caller changes what a deployed function does without touching the secret.
async fn function_fails_at(router: &MultiServiceRouter, case: &str, step: &str) {
    let (status, updated) = lambda(
        router,
        "PUT",
        &format!("/2015-03-31/functions/{FUNCTION_NAME}/configuration"),
        Some(json!({ "Environment": { "Variables": { FAIL_AT_VARIABLE: step } } })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UpdateFunctionConfiguration, got {status} with \
         {updated}"
    );
    assert_eq!(
        updated["Environment"]["Variables"][FAIL_AT_VARIABLE].as_str(),
        Some(step),
        "case {case:?}: expected the function to report the step it cannot complete, got {updated}"
    );
    wait_until_invocable(router, case).await;
}

/// Call one Secrets Manager operation in this suite's Region, requiring the 200 it answers.
async fn sm_ok(router: &MultiServiceRouter, case: &str, operation: &str, request: &Value) -> Value {
    served_in(router, case, REGION, operation, request).await
}

/// The same, requiring a refusal: its status, wire error code and message.
async fn sm_err(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    request: &Value,
) -> (u16, String, String) {
    refused_in(router, case, REGION, operation, request).await
}

/// The versions a staging map reports `stage` on.
fn holders(stages: &BTreeMap<String, Vec<String>>, stage: &str) -> Vec<String> {
    stages
        .iter()
        .filter(|(_, labels)| labels.iter().any(|label| label == stage))
        .map(|(version, _)| version.clone())
        .collect()
}

/// Every version id `ListSecretVersionIds` reports for the secret, the ones no staging label
/// reaches any more included ("specifies whether to include versions of secrets that don't have
/// any staging labels attached to them", `ListSecretVersionIdsRequest$IncludeDeprecated`).
async fn stored_versions(router: &MultiServiceRouter, case: &str, secret_id: &str) -> Vec<String> {
    let listed = sm_ok(
        router,
        case,
        "ListSecretVersionIds",
        &json!({ "SecretId": secret_id, "IncludeDeprecated": true }),
    )
    .await;
    let mut ids: Vec<String> = listed["Versions"]
        .as_array()
        .map(|versions| {
            versions
                .iter()
                .filter_map(|version| version["VersionId"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids
}

/// Poll `DescribeSecret` until the rotation of `secret_id` has FINISHED — stamped with the instant
/// it finished ("the last date and time that Secrets Manager rotated the secret",
/// `DescribeSecretResponse$LastRotatedDate`) — and answer the last answer read.
async fn wait_for_the_rotation_to_finish(
    router: &MultiServiceRouter,
    case: &str,
    secret_id: &str,
) -> Value {
    let mut last = Value::Null;
    for _ in 0..POLLS {
        last = described(router, case, secret_id).await;
        if last.get("LastRotatedDate").is_some() {
            return last;
        }
        tokio::time::sleep(POLL).await;
    }
    last
}

/// Assert that the rotation configuration `described` reports is the one the row's FIRST call
/// stored: the re-drive named none of it, so none of it may have changed.
fn configuration_is_the_stored_one(described: &Value, case: &str, function_arn: &str, phase: &str) {
    assert_eq!(
        described["RotationLambdaARN"].as_str(),
        Some(function_arn),
        "case {case:?}: expected the stored rotation function {phase}, got {described}"
    );
    assert_eq!(
        described["RotationRules"]["AutomaticallyAfterDays"].as_u64(),
        Some(SCHEDULE_DAYS),
        "case {case:?}: expected the stored schedule {phase}, got {described}"
    );
    assert_eq!(
        described["RotationEnabled"].as_bool(),
        Some(true),
        "case {case:?}: expected rotation to stay enabled {phase}, got {described}"
    );
}

/// RULE: a rotation that ended unfinished is re-driven by the call naming its candidate and by no
/// other, and such a call carries no configuration: it runs the function the secret stores, as the
/// client has since repaired it, and leaves the stored configuration alone.
#[tokio::test]
async fn a_re_driven_rotation_runs_the_stored_function_the_client_repaired() {
    let router = build_router();
    let function_arn = deployed_function(&router, "redrivestoredstack").await;

    for row in ROWS {
        let case = row.case;
        // The function cannot complete this row's step, which is what makes the rotation below
        // end unfinished.
        function_fails_at(&router, case, row.fail_at).await;

        let name = format!("stored-configuration-credentials-{}", row.id);
        let created = sm_ok(
            &router,
            case,
            "CreateSecret",
            &json!({
                "Name": name.clone(),
                "ClientRequestToken": token(row.id),
                "SecretString": INITIAL_VALUE,
            }),
        )
        .await;
        let original = created["VersionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no VersionId in {created}"))
            .to_string();

        // The FIRST call is the one that configures the rotation: the function and the schedule
        // the re-drive will name none of.
        let candidate = token(&format!("cand{}", row.id));
        let started = sm_ok(
            &router,
            case,
            "RotateSecret",
            &json!({
                "SecretId": name.clone(),
                "ClientRequestToken": candidate.clone(),
                "RotationLambdaARN": function_arn.clone(),
                "RotationRules": { "AutomaticallyAfterDays": SCHEDULE_DAYS },
                "RotateImmediately": true,
            }),
        )
        .await;
        assert_eq!(
            started["VersionId"].as_str(),
            Some(candidate.as_str()),
            "case {case:?}: expected the rotation to stake the token the call named, got {started}"
        );

        let stopped_after = row.stopped_after();
        let wedged = wait_for_steps(&router, case, &name, stopped_after.len()).await;
        assert_eq!(
            recorded_steps(&wedged),
            stopped_after,
            "case {case:?}: expected the rotation to stop after {stopped_after:?}, got {wedged}"
        );
        let wedged_stages = stages_of(&wedged);
        assert_eq!(
            wedged_stages,
            BTreeMap::from([
                (original.clone(), vec![CURRENT_STAGE.to_string()]),
                (candidate.clone(), vec![PENDING_STAGE.to_string()]),
            ]),
            "case {case:?}: expected the candidate outstanding beside an untouched \
             {CURRENT_STAGE}, got {wedged}"
        );
        assert!(
            wedged.get("LastRotatedDate").is_none(),
            "case {case:?}: expected no LastRotatedDate for a rotation that did not finish, got \
             {wedged}"
        );
        configuration_is_the_stored_one(&wedged, case, &function_arn, "once the rotation stopped");

        // The boundary: a token that is not the outstanding candidate's own asks for a rotation
        // BESIDE that one, whether or not the secret already holds a version under it — and is
        // refused with the fixed sentence, staking and minting nothing.
        let intruder = token(&format!("fresh{}", row.id));
        let others = [
            ("a token no version of this secret is stored under", intruder.clone()),
            (
                "the id of the version AWSCURRENT is on",
                original.clone(),
            ),
        ];
        for (what, other) in &others {
            let (status, code, message) = sm_err(
                &router,
                case,
                "RotateSecret",
                &json!({
                    "SecretId": name.clone(),
                    "ClientRequestToken": other.clone(),
                    "RotateImmediately": true,
                }),
            )
            .await;
            assert_eq!(
                (status, code.as_str()),
                (400, "InvalidRequestException"),
                "case {case:?}: expected 400 InvalidRequestException for {what}, got {status} \
                 {code:?} with {message:?}"
            );
            assert_eq!(
                message, ROTATION_IN_PROGRESS,
                "case {case:?}: expected {ROTATION_IN_PROGRESS:?} for {what}, got {message:?}"
            );
            assert_eq!(
                stages_of(&described(&router, case, &name).await),
                wedged_stages,
                "case {case:?}: expected the call refused for {what} to stake nothing, got a \
                 changed staging map"
            );
        }

        // The repair a client makes to the FUNCTION, which is where the step that failed lives.
        function_fails_at(&router, case, "").await;

        // The re-drive: the secret, the candidate's own token, and nothing else — so what drives
        // the rotation can only be what the secret stores.
        //
        // The call is made until it is SERVED rather than once: the rotation ends when the
        // invocation its function raised in returns to the service, which is a moment after the
        // step record waited for above, so a call arriving in between is still refused for a
        // rotation in flight. The ceiling is there only so the row cannot hang; a refused call
        // stakes nothing, so repeating it changes nothing.
        let request = json!({
            "SecretId": name.clone(),
            "ClientRequestToken": candidate.clone(),
            "RotateImmediately": true,
        });
        let mut re_driven = Value::Null;
        for look in 0..POLLS {
            let (status, answered) = secretsmanager(&router, REGION, "RotateSecret", &request).await;
            if status == 200 {
                re_driven = answered;
                break;
            }
            assert!(
                look + 1 < POLLS,
                "case {case:?}: the rotation's own token never re-drove it, last answer {status} \
                 with {answered}"
            );
            tokio::time::sleep(POLL).await;
        }
        assert_eq!(
            re_driven["VersionId"].as_str(),
            Some(candidate.as_str()),
            "case {case:?}: expected the re-drive to answer the candidate it names, got {re_driven}"
        );
        assert_eq!(
            re_driven["Name"].as_str(),
            Some(name.as_str()),
            "case {case:?}: expected the re-drive to answer the secret's own name, got {re_driven}"
        );

        // It runs the stored function again, this time through `finishSecret`, so the candidate is
        // promoted and the rotation is stamped as finished.
        let finished = wait_for_the_rotation_to_finish(&router, case, &name).await;
        assert!(
            finished.get("LastRotatedDate").is_some(),
            "case {case:?}: expected the re-driven rotation to finish, got {finished}"
        );
        let after_the_re_drive = row.after_the_re_drive();
        assert_eq!(
            recorded_steps(&finished),
            after_the_re_drive,
            "case {case:?}: expected the steps {after_the_re_drive:?}, got {finished}"
        );
        // The candidate may keep `AWSPENDING` as well, because that is the shape of a rotation
        // that SUCCEEDED: "when rotation is successful, the AWSPENDING staging label might be
        // attached to the same version as the AWSCURRENT version, or it might not be attached to
        // any version" (`RotateSecret`, Secrets Manager's Smithy model) — what it may not do is
        // sit on another version, which is the shape of a rotation still outstanding.
        let promoted = stages_of(&finished);
        assert_eq!(
            holders(&promoted, CURRENT_STAGE),
            vec![candidate.clone()],
            "case {case:?}: expected {CURRENT_STAGE} on the promoted candidate alone, got \
             {finished}"
        );
        assert_eq!(
            holders(&promoted, PREVIOUS_STAGE),
            vec![original.clone()],
            "case {case:?}: expected {PREVIOUS_STAGE} on the version the rotation replaced, got \
             {finished}"
        );
        assert!(
            holders(&promoted, PENDING_STAGE)
                .iter()
                .all(|version| *version == candidate),
            "case {case:?}: expected no candidate outstanding once the rotation finished, got \
             {finished}"
        );
        // The configuration the re-drive named none of is the one it ran under and the one it
        // left stored.
        configuration_is_the_stored_one(&finished, case, &function_arn, "after the re-drive");

        // What a client reads is that candidate's own value, and the secret holds no version
        // minted beside it: the token is what kept the rotation to one candidate.
        let read = sm_ok(
            &router,
            case,
            "GetSecretValue",
            &json!({ "SecretId": name.clone(), "VersionStage": CURRENT_STAGE }),
        )
        .await;
        let expected_value = format!("{INITIAL_VALUE}{ROTATED_SUFFIX}");
        assert_eq!(
            (read["VersionId"].as_str(), read["SecretString"].as_str()),
            (Some(candidate.as_str()), Some(expected_value.as_str())),
            "case {case:?}: expected the candidate's own value served, got {read}"
        );
        let mut expected_versions = vec![original.clone(), candidate.clone()];
        expected_versions.sort();
        assert_eq!(
            stored_versions(&router, case, &name).await,
            expected_versions,
            "case {case:?}: expected the two versions of the one rotation, and nothing minted for \
             {intruder:?}"
        );
    }
}
