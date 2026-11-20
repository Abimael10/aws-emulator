// SPDX-License-Identifier: CC0-1.0

//! A rotation stops at the step its rotation function could not complete, and every step it
//! DID run is readable in that function's own CloudWatch Logs group.
//!
//! The rule under test: **Secrets Manager drives a started rotation by invoking the configured
//! rotation function once per step, in the order `createSecret`, `setSecret`, `testSecret`,
//! `finishSecret` ("Secrets Manager calls the Lambda rotation function four times, each time
//! with a different value for the Step parameter"), stops at the first step whose invocation
//! does not complete, and writes nothing of its own to the staging map — so `AWSCURRENT` moves
//! only where the function's own `finishSecret` step moved it. The output each of those
//! invocations wrote is delivered to the function's log group ("By default, Lambda functions
//! send logs to a default log group named `/aws/lambda/<function name>`",
//! `LoggingConfig$LogGroup`), which is the only window a client has onto which step its
//! function failed at.**
//!
//! That is the whole safety property of rotation: an untested credential is never made current,
//! and a client can tell a rotation that stopped safely from one that never started.
//!
//! The rows span what the rule generalises over — WHICH step the function cannot complete:
//! each of the four in turn, plus the function that completes them all. A row whose function
//! raises at step *k* must show the markers of steps 1..=k ONCE EACH in its log group, none past
//! it and none of them a second time, and a staging map still naming exactly one `AWSCURRENT` on
//! the version the secret was created with; the row that raises at none promotes the candidate
//! its own `createSecret` staged. "Once each" is half the rule: a step the function RAN and
//! answered is answered, so the steps before it are not driven through again behind a client
//! reading that log to learn which step failed.
//!
//! Four crates have to agree for the rule to hold — CloudFormation deploys the rotation
//! functions and the role they run as, IAM answers for that role and for the grant Secrets
//! Manager invokes them under, Lambda runs each invocation and delivers its log, Secrets Manager
//! drives the protocol — so the suite lives here. Every wait below looks for the STATE it waits
//! on, and its ceiling is there only so the suite cannot hang.

use std::collections::BTreeMap;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode, opt_element, outputs_of};
use crate::secretsmanager_rotation_function_is_tested_and_validated::{
    build_router, described, secretsmanager_ok as served_in, stages_of, token, CURRENT_STAGE,
    POLL, POLLS, REGION, ROTATION_STEPS,
};

/// The staging label a rotation's candidate carries until its `finishSecret` step promotes it.
const PENDING_STAGE: &str = "AWSPENDING";

/// How many looks this suite keeps watching a log group for a record that must NOT arrive.
///
/// The window is counted in LOOKS rather than in time, so it stretches with the host: every look
/// is a round trip through the router, and the record it watches for would be owed a step's worth
/// of invocation after the one already read. What it asserts over the window is the ABSENCE of a
/// record, so a slow host can only make it miss a violation, never invent one.
const QUIET_LOOKS: usize = 24;

/// The label the version a promotion replaced carries afterwards.
const PREVIOUS_STAGE: &str = "AWSPREVIOUS";

/// The value every row's secret is created with: a row whose rotation stopped must still serve
/// it, and the row that promotes must serve it with the suffix its function appended.
const INITIAL_VALUE: &str = "the-credential-before-any-rotation";

/// What the rotation function appends to the value it stages on `createSecret`.
const ROTATED_SUFFIX: &str = "-rotated";

/// The service principal Secrets Manager invokes a rotation function as ("ensure that the
/// function policy grants access to the principal `secretsmanager.amazonaws.com`", Secrets
/// Manager User Guide, *Permissions for rotation*).
const SERVICE_PRINCIPAL: &str = "secretsmanager.amazonaws.com";

/// How long one invocation of the rotation function is given, as the function's own `Timeout`:
/// well past what a run needs, because what is under test is the rotation protocol rather than
/// how fast an interpreter gets through a handler.
const FUNCTION_TIMEOUT: u64 = 30;

/// The prefix a function's default log group is named with: "By default, Lambda functions send
/// logs to a default log group named `/aws/lambda/<function name>`" (`LoggingConfig$LogGroup`).
const DEFAULT_GROUP_PREFIX: &str = "/aws/lambda/";

/// What every invocation of the rotation function writes to its log before it does anything
/// else, and which this suite reads the steps back out of. The marker is the function's own
/// output, not a record the platform frames.
const STEP_MARKER: &str = "rotation step ";

/// The environment variable each row's function reads the step it cannot complete from: the
/// rows deploy ONE source and differ only in this value.
const FAIL_AT_VARIABLE: &str = "FAIL_AT";

/// The rotation function every row rotates through, written the way the AWS rotation templates
/// are: it RECORDS the step it was invoked with first of all, then does that step's own work
/// through Secrets Manager's own operations — `createSecret` stages a candidate under
/// `AWSPENDING`, `testSecret` reads it back, `finishSecret` moves `AWSCURRENT` onto it — and
/// raises on the step [`FAIL_AT_VARIABLE`] names, which is the only thing the rows differ in.
const ROTATION_FUNCTION: &str = r#"
import os

import boto3
from botocore.exceptions import ClientError

FAIL_AT = os.environ.get("FAIL_AT", "")
SUFFIX = "-rotated"


def staged_value(client, arn, token):
    try:
        return client.get_secret_value(SecretId=arn, VersionId=token)
    except ClientError:
        return None


def lambda_handler(event, context):
    arn = event["SecretId"]
    token = event["ClientRequestToken"]
    step = event["Step"]
    print("rotation step %s" % step)
    if step == FAIL_AT:
        raise RuntimeError("this rotation function cannot complete the %s step" % step)
    client = boto3.client("secretsmanager")
    metadata = client.describe_secret(SecretId=arn)
    versions = metadata.get("VersionIdsToStages", {})
    if "AWSCURRENT" in versions.get(token, []):
        return {"step": step, "already_current": True}
    if step == "createSecret":
        if staged_value(client, arn, token) is None:
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
        client.get_secret_value(SecretId=arn, VersionId=token)
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

/// One row: the step this row's function cannot complete, and the function it is deployed as.
struct Row {
    case: &'static str,
    /// The step the function raises on, or `""` for the function that completes every step.
    fail_at: &'static str,
    /// The logical id and function name this row's function is deployed under.
    id: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "a function that completes every step",
        fail_at: "",
        id: "rotatorcomplete",
    },
    Row {
        case: "a function that raises in createSecret",
        fail_at: "createSecret",
        id: "rotatorcreate",
    },
    Row {
        case: "a function that raises in setSecret",
        fail_at: "setSecret",
        id: "rotatorset",
    },
    Row {
        case: "a function that raises in testSecret",
        fail_at: "testSecret",
        id: "rotatortest",
    },
    Row {
        case: "a function that raises in finishSecret",
        fail_at: "finishSecret",
        id: "rotatorfinish",
    },
];

impl Row {
    /// How many of [`ROTATION_STEPS`] this row's function gets THROUGH, the raising one
    /// included: the protocol stops at the first step that did not complete.
    fn steps_reached(&self) -> usize {
        ROTATION_STEPS
            .iter()
            .position(|step| *step == self.fail_at)
            .map_or(ROTATION_STEPS.len(), |at| at + 1)
    }

    /// Whether this row's rotation promotes its candidate, which only the function that
    /// completes `finishSecret` does.
    fn promotes(&self) -> bool {
        self.fail_at.is_empty()
    }

    /// The log group this row's invocations are delivered to.
    fn log_group(&self) -> String {
        format!("{DEFAULT_GROUP_PREFIX}{}", self.id)
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

/// Call one CloudWatch Logs operation over its awsJson1_1 wire, with whatever status it
/// answered: a group no delivery has created yet is a refusal this suite reads THROUGH, because
/// it is exactly what a client polling for a line that has not come yet sees.
async fn logs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("logs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Logs_20140328.{operation}"))
        .header("authorization", authorization("logs"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// The steps the function recorded in `group`, in the order its invocations wrote them.
async fn logged_steps(router: &MultiServiceRouter, group: &str) -> Vec<String> {
    let (status, answered) = logs(router, "FilterLogEvents", &json!({ "logGroupName": group })).await;
    if status != 200 {
        return Vec::new();
    }
    answered["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|event| event["message"].as_str())
        .filter_map(|message| {
            message
                .split(STEP_MARKER)
                .nth(1)
                .map(|tail| tail.trim().to_string())
        })
        .collect()
}

/// Poll `group` until the function has logged `wanted` steps, and answer the steps it then
/// holds. Nothing here sleeps on an invocation's behalf: the invocations are the service's own
/// work, and this looks for the record of them.
async fn wait_for_logged_steps(
    router: &MultiServiceRouter,
    group: &str,
    wanted: usize,
) -> Vec<String> {
    let mut seen = Vec::new();
    for _ in 0..POLLS {
        seen = logged_steps(router, group).await;
        if seen.len() >= wanted {
            return seen;
        }
        tokio::time::sleep(POLL).await;
    }
    seen
}

/// Call one Secrets Manager operation in this suite's Region, requiring the 200 it answers.
async fn secretsmanager_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    request: &Value,
) -> Value {
    served_in(router, case, REGION, operation, request).await
}

/// The versions `DescribeSecret` reports the staging label `stage` on.
fn staged_versions(described: &Value, stage: &str) -> Vec<String> {
    stages_of(described)
        .into_iter()
        .filter(|(_, labels)| labels.iter().any(|label| label == stage))
        .map(|(version, _)| version)
        .collect()
}

/// The template deploying every row's rotation function: ONE execution role that may read and
/// write the secrets and write its own logs, one function per row differing only in the step it
/// raises on, and the grant that lets Secrets Manager invoke each.
fn functions_template() -> String {
    let mut resources = serde_json::Map::new();
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
                        "Action": "sts:AssumeRole"
                    }]
                },
                "Policies": [{
                    "PolicyName": "rotate-the-secret-and-write-the-log",
                    "PolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Action": ["secretsmanager:*", "logs:*"],
                            "Resource": "*"
                        }]
                    }
                }]
            }
        }),
    );
    let mut outputs = serde_json::Map::new();
    for row in ROWS {
        let logical = format!("Function{}", row.id);
        resources.insert(
            logical.clone(),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": row.id,
                    "Runtime": "python3.12",
                    "Handler": "index.lambda_handler",
                    "Role": { "Fn::GetAtt": ["RotationRole", "Arn"] },
                    "Timeout": FUNCTION_TIMEOUT,
                    "Environment": { "Variables": { FAIL_AT_VARIABLE: row.fail_at } },
                    "Code": { "ZipFile": ROTATION_FUNCTION }
                }
            }),
        );
        resources.insert(
            format!("Permission{}", row.id),
            json!({
                "Type": "AWS::Lambda::Permission",
                "Properties": {
                    "Action": "lambda:InvokeFunction",
                    "FunctionName": { "Fn::GetAtt": [logical.clone(), "Arn"] },
                    "Principal": SERVICE_PRINCIPAL
                }
            }),
        );
        outputs.insert(
            format!("Arn{}", row.id),
            json!({ "Value": { "Fn::GetAtt": [logical, "Arn"] } }),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "the rotation functions this suite's rotations are driven through",
        "Resources": resources,
        "Outputs": outputs,
    })
    .to_string()
}

/// Deploy that template, and answer each row's function ARN by its id.
async fn deployed_functions(router: &MultiServiceRouter, stack: &str) -> BTreeMap<String, String> {
    let template = functions_template();
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
        let (_, events) =
            cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
        elements(&events, "ResourceStatusReason")
    };
    assert_eq!(
        last.0, "CREATE_COMPLETE",
        "setup: expected the rotation functions' stack to complete, got {:?} with {} and \
         resource failures {reasons:?}",
        last.0, last.1
    );
    let published = outputs_of(&last.1);
    ROWS.iter()
        .map(|row| {
            let key = format!("Arn{}", row.id);
            let arn = published
                .get(&key)
                .unwrap_or_else(|| panic!("setup: no {key} output in {}", last.1))
                .clone();
            (row.id.to_string(), arn)
        })
        .collect()
}

/// RULE: a started rotation runs its function once per step in the documented order, stops at
/// the first step the function could not complete, leaves the staging map to that function's own
/// `finishSecret` step, and leaves every step it ran readable in the function's log group.
#[tokio::test]
async fn a_rotation_stops_at_the_step_its_function_cannot_complete() {
    let router = build_router();
    let functions = deployed_functions(&router, "rotationstepstack").await;

    for row in ROWS {
        let case = row.case;
        let function = &functions[row.id];
        let name = format!("stepwise-credentials-{}", row.id);
        let created = secretsmanager_ok(
            &router,
            case,
            "CreateSecret",
            &json!({
                "Name": name,
                "ClientRequestToken": token(row.id),
                "SecretString": INITIAL_VALUE,
            }),
        )
        .await;
        let current = created["VersionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no VersionId in {created}"))
            .to_string();

        let candidate = token(&format!("cand{}", row.id));
        let started = secretsmanager_ok(
            &router,
            case,
            "RotateSecret",
            &json!({
                "SecretId": name,
                "ClientRequestToken": candidate,
                "RotationLambdaARN": function,
                "RotationRules": { "AutomaticallyAfterDays": 30 },
                "RotateImmediately": true,
            }),
        )
        .await;
        assert_eq!(
            started["VersionId"].as_str(),
            Some(candidate.as_str()),
            "case {case:?}: expected the rotation to stake the token the call named, got {started}"
        );

        // The rotation runs its function ONCE per step and stops at the step the function did
        // not complete, so the row waits for that step's record and then WATCHES the group: a
        // step the rotation went on to run, and a pass of the protocol it started over, would
        // land inside the window. A rotation that COMPLETED has nothing left that could run, so
        // that row reads the group once.
        let reached = row.steps_reached();
        let expected_order: Vec<String> = ROTATION_STEPS[..reached]
            .iter()
            .map(|step| (*step).to_string())
            .collect();
        let steps = wait_for_logged_steps(&router, &row.log_group(), reached).await;
        assert_eq!(
            steps, expected_order,
            "case {case:?}: expected the steps {expected_order:?} once each, in that order, got \
             {steps:?} in {}",
            row.log_group()
        );
        for _ in 0..if row.promotes() { 0 } else { QUIET_LOOKS } {
            tokio::time::sleep(POLL).await;
            let watched = logged_steps(&router, &row.log_group()).await;
            assert_eq!(
                watched, expected_order,
                "case {case:?}: expected the function to have been invoked once per step and no \
                 more, got {watched:?} in {}",
                row.log_group()
            );
        }
        let steps = logged_steps(&router, &row.log_group()).await;
        for skipped in &ROTATION_STEPS[reached..] {
            assert!(
                !steps.iter().any(|step| step == skipped),
                "case {case:?}: expected the protocol to stop before {skipped:?}, got {steps:?}"
            );
        }

        // What the rotation left: the candidate is promoted only where the function's own
        // `finishSecret` step promoted it, and exactly one version carries `AWSCURRENT` either
        // way.
        let described = described(&router, case, &name).await;
        let holders = staged_versions(&described, CURRENT_STAGE);
        let expected_current = if row.promotes() { &candidate } else { &current };
        assert_eq!(
            holders,
            vec![expected_current.clone()],
            "case {case:?}: expected {CURRENT_STAGE} on exactly {expected_current:?}, got \
             {described}"
        );
        if row.promotes() {
            assert_eq!(
                staged_versions(&described, PREVIOUS_STAGE),
                vec![current.clone()],
                "case {case:?}: expected {PREVIOUS_STAGE} on the version the rotation replaced, \
                 got {described}"
            );
        } else {
            // The candidate stays a candidate: the id the rotation staked, whether its own
            // `createSecret` step has created that version yet or not.
            assert_eq!(
                staged_versions(&described, PENDING_STAGE),
                vec![candidate.clone()],
                "case {case:?}: expected the candidate to keep only {PENDING_STAGE}, got \
                 {described}"
            );
        }

        // ... and what a client reads is the value that version holds.
        let read = secretsmanager_ok(
            &router,
            case,
            "GetSecretValue",
            &json!({ "SecretId": name, "VersionStage": CURRENT_STAGE }),
        )
        .await;
        let expected_value = if row.promotes() {
            format!("{INITIAL_VALUE}{ROTATED_SUFFIX}")
        } else {
            INITIAL_VALUE.to_string()
        };
        assert_eq!(
            read["SecretString"].as_str(),
            Some(expected_value.as_str()),
            "case {case:?}: expected the value {expected_value:?}, got {read}"
        );
        assert_eq!(
            read["VersionId"].as_str(),
            Some(expected_current.as_str()),
            "case {case:?}: expected the served version {expected_current:?}, got {read}"
        );
    }
}
