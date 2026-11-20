// SPDX-License-Identifier: CC0-1.0

//! The four-step rotation protocol on the emulator a client STARTS: the built binary, on its
//! own port, with request authentication on, reached over TLS with SigV4-signed calls.
//!
//! The rule under test: **`RotateSecret` with `RotateImmediately` drives the rotation function
//! through the documented steps and stops at the one the function could not complete, so a
//! client watching that function's own log group — "a default log group named
//! `/aws/lambda/<function name>`" (`LoggingConfig$LogGroup`) — reads `createSecret`, `setSecret`
//! and `testSecret` and never `finishSecret`, and the secret it was rotating still serves the
//! version it served before: exactly one version carries `AWSCURRENT` and it is the one the
//! create made, while the untested candidate keeps `AWSPENDING` alone.**
//!
//! The sibling suite `secretsmanager_rotation_stops_at_the_step_its_function_raises_in` spans
//! WHICH step the function stops at, on a router assembled inside the test process. This suite
//! pins the same protocol on the emulator as it is STARTED, because that is the configuration a
//! client runs: the credentials are verified, each call arrives over its own TLS connection, and
//! one process serves the client's calls, the rotation's own invocations and the delivery of
//! each invocation's log. A rotation that runs in process and not there is a failure a test
//! reports rather than one only a client sees. ONE rotation is driven here, since starting the
//! binary costs what starting the emulator costs.
//!
//! Every wait below looks for the STATE it waits on, and its ceiling is there only so the suite
//! cannot hang. The staging map is read with the reader the sibling suite owns, so there is one
//! reading of one in this crate.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::cfn_deployed_pipeline_on_a_started_emulator::Started;
use crate::query_wire::{elements, opt_element, outputs_of};
use crate::secretsmanager_rotation_function_is_tested_and_validated::{
    stages_of, CURRENT_STAGE, ROTATION_STEPS,
};

/// How long the deployment and the rotation are waited for, and how often this suite looks
/// while waiting. Every wait is on a CONDITION; the spans are generous, so a loaded host spends
/// none of them.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(200);

/// How many looks this suite keeps watching the function's log group for a step that must NOT
/// run. The window is counted in LOOKS rather than in time, so it stretches with the host, and
/// it asserts the ABSENCE of a record: a slow host can only make it miss a violation rather than
/// invent one.
const QUIET_LOOKS: usize = 15;

/// The `StackStatus` the deployment must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The function this suite's rotation is driven through, and the log group it writes to.
const FUNCTION: &str = "startedrotator";

/// The staging label the rotation's candidate carries while it is only a candidate.
const PENDING_STAGE: &str = "AWSPENDING";

/// The value the secret is created with, which a rotation that stopped must still serve.
const INITIAL_VALUE: &str = "the-credential-before-any-rotation";

/// The id the create stakes its version on, and the one the rotation stakes its candidate on:
/// `ClientRequestTokenType` is modelled with a length of 32..64.
const CURRENT_TOKEN: &str = "started-rotation-current-0000000";
const CANDIDATE_TOKEN: &str = "started-rotation-candidate-00000";

/// What every invocation of the rotation function writes to its log before it does anything
/// else, and which this suite reads the steps back out of.
const STEP_MARKER: &str = "rotation step ";

/// The rotation function: it records the step it was invoked with first of all, then does that
/// step's own work through Secrets Manager's own operations — and raises on `testSecret`, which
/// is the step this suite's rotation must stop at.
const ROTATION_FUNCTION: &str = r#"import logging

import boto3

logger = logging.getLogger()
logger.setLevel(logging.INFO)


def handler(event, context):
    arn = event["SecretId"]
    token = event["ClientRequestToken"]
    step = event["Step"]
    logger.info("rotation step %s" % step)
    client = boto3.client("secretsmanager")
    metadata = client.describe_secret(SecretId=arn)
    versions = metadata["VersionIdsToStages"]
    if token not in versions:
        raise ValueError("Secret version %s has no stage for rotation" % token)
    if step == "createSecret":
        try:
            client.get_secret_value(SecretId=arn, VersionId=token, VersionStage="AWSPENDING")
        except client.exceptions.ResourceNotFoundException:
            current = client.get_secret_value(SecretId=arn, VersionStage="AWSCURRENT")
            client.put_secret_value(
                SecretId=arn,
                ClientRequestToken=token,
                SecretString=current["SecretString"] + "-rotated",
                VersionStages=["AWSPENDING"],
            )
    elif step == "setSecret":
        pass
    elif step == "testSecret":
        raise ValueError("testSecret: the candidate did not pass its test")
    elif step == "finishSecret":
        previous = None
        for version in versions:
            if "AWSCURRENT" in versions[version]:
                previous = version
        client.update_secret_version_stage(
            SecretId=arn,
            VersionStage="AWSCURRENT",
            MoveToVersionId=token,
            RemoveFromVersionId=previous,
        )
    return {"step": step}
"#;

/// The stack deploying that function: the execution role it runs as — which may read and write
/// the secret and write its own log — the function itself, and the grant that lets Secrets
/// Manager invoke it.
fn rotation_stack() -> Value {
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
                            "Action": "sts:AssumeRole",
                        }],
                    },
                    "Policies": [{
                        "PolicyName": "rotate-the-secret-and-write-the-log",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": ["secretsmanager:*", "logs:*"],
                                "Resource": "*",
                            }],
                        },
                    }],
                },
            },
            "Rotator": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": { "Fn::GetAtt": ["RotationRole", "Arn"] },
                    "Timeout": 30,
                    "Code": { "ZipFile": ROTATION_FUNCTION },
                },
            },
            "InvokePermission": {
                "Type": "AWS::Lambda::Permission",
                "Properties": {
                    "FunctionName": { "Fn::GetAtt": ["Rotator", "Arn"] },
                    "Action": "lambda:InvokeFunction",
                    "Principal": "secretsmanager.amazonaws.com",
                },
            },
        },
        "Outputs": {
            "FunctionArn": { "Value": { "Fn::GetAtt": ["Rotator", "Arn"] } },
        },
    })
}

/// One Secrets Manager call over its awsJson1_1 wire, as its status and parsed body.
fn secretsmanager(started: &Started, operation: &str, body: &Value) -> (u16, Value) {
    let (status, text) = started.rpc(
        "secretsmanager",
        &format!("secretsmanager.{operation}"),
        "application/x-amz-json-1.1",
        body,
    );
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

/// The same, requiring the 200 the operation answers.
fn secretsmanager_ok(started: &Started, operation: &str, body: &Value) -> Value {
    let (status, answered) = secretsmanager(started, operation, body);
    assert_eq!(
        status, 200,
        "expected HTTP 200 from {operation}, got {status} with {answered}"
    );
    answered
}

/// The steps the rotation function has recorded in its log group, in the order its invocations
/// wrote them. A group no delivery has created yet is read THROUGH: it is what a client polling
/// for a record that has not come yet sees.
fn logged_steps(started: &Started) -> Vec<String> {
    let group = format!("/aws/lambda/{FUNCTION}");
    let (status, text) = started.rpc(
        "logs",
        "Logs_20140328.FilterLogEvents",
        "application/x-amz-json-1.1",
        &json!({ "logGroupName": group }),
    );
    if status != 200 {
        return Vec::new();
    }
    let answered: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
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

/// RULE: see the module docs — on the emulator a client starts, an immediate rotation whose
/// function raises in `testSecret` runs the steps up to that one, never `finishSecret`, and
/// leaves the secret serving the version it served before.
#[test]
fn a_started_emulator_drives_the_rotation_and_stops_at_the_failing_step() {
    let emulator = Started::start();
    let stack = "startedrotationstack";
    let (status, body) = emulator.cloudformation(
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", &rotation_stack().to_string()),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    );
    assert_eq!(
        status, 200,
        "setup: expected HTTP 200 from CreateStack, got {status} with {body}"
    );
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    let deployed = loop {
        let (status, body) = emulator.cloudformation("DescribeStacks", &[("StackName", stack)]);
        assert_eq!(
            status, 200,
            "setup: expected HTTP 200 from DescribeStacks, got {status} with {body}"
        );
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        if reported == CREATE_COMPLETE {
            break body;
        }
        if Instant::now() >= giving_up_at {
            let (_, events) =
                emulator.cloudformation("DescribeStackEvents", &[("StackName", stack)]);
            panic!(
                "setup: the rotation function's stack reached {reported:?}, not \
                 {CREATE_COMPLETE}: {body}\nresource failures: {:?}\nthe emulator's log:\n{}",
                elements(&events, "ResourceStatusReason"),
                emulator.log()
            );
        }
        std::thread::sleep(LOOK_INTERVAL);
    };
    let function = outputs_of(&deployed)
        .get("FunctionArn")
        .cloned()
        .unwrap_or_else(|| panic!("setup: no FunctionArn output in {deployed}"));

    let created = secretsmanager_ok(
        &emulator,
        "CreateSecret",
        &json!({
            "Name": "started-rotated-credentials",
            "ClientRequestToken": CURRENT_TOKEN,
            "SecretString": INITIAL_VALUE,
        }),
    );
    let secret = created["ARN"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: no ARN in {created}"))
        .to_string();

    let started = secretsmanager_ok(
        &emulator,
        "RotateSecret",
        &json!({
            "SecretId": secret,
            "ClientRequestToken": CANDIDATE_TOKEN,
            "RotationLambdaARN": function,
            "RotationRules": { "AutomaticallyAfterDays": 30 },
            "RotateImmediately": true,
        }),
    );
    assert_eq!(
        started["VersionId"].as_str(),
        Some(CANDIDATE_TOKEN),
        "expected the rotation to stake the token the call named, got {started}"
    );

    // The steps the function ran, and the step it was never invoked for: the rotation stops at
    // the one it could not complete, so the watch below looks for `finishSecret` arriving late
    // rather than concluding from one look.
    let stopped_at: Vec<String> = ROTATION_STEPS[..3]
        .iter()
        .map(|step| (*step).to_string())
        .collect();
    let mut steps;
    loop {
        steps = logged_steps(&emulator);
        if steps == stopped_at {
            break;
        }
        if Instant::now() >= giving_up_at {
            panic!(
                "expected the rotation to log {stopped_at:?}, got {steps:?}\nthe emulator's \
                 log:\n{}",
                emulator.log()
            );
        }
        std::thread::sleep(LOOK_INTERVAL);
    }
    assert_eq!(
        steps, stopped_at,
        "expected the rotation to log {stopped_at:?}, got {steps:?}"
    );
    for _ in 0..QUIET_LOOKS {
        std::thread::sleep(LOOK_INTERVAL);
        let watched = logged_steps(&emulator);
        assert_eq!(
            watched, stopped_at,
            "expected the rotation to stop at {:?} and invoke the function once per step, got \
             {watched:?}",
            ROTATION_STEPS[2]
        );
    }

    // What the rotation left: the version the create made is still the only one carrying
    // AWSCURRENT, and the untested candidate carries AWSPENDING alone.
    let described = secretsmanager_ok(&emulator, "DescribeSecret", &json!({ "SecretId": secret }));
    assert_eq!(
        stages_of(&described),
        BTreeMap::from([
            (CURRENT_TOKEN.to_string(), vec![CURRENT_STAGE.to_string()]),
            (CANDIDATE_TOKEN.to_string(), vec![PENDING_STAGE.to_string()]),
        ]),
        "expected the staging map untouched by the rotation, got {described}"
    );
    assert!(
        described.get("LastRotatedDate").is_none(),
        "expected an unfinished rotation to leave LastRotatedDate unset, got {described}"
    );

    // ... and a client still reads the value it read before.
    let read = secretsmanager_ok(&emulator, "GetSecretValue", &json!({ "SecretId": secret }));
    assert_eq!(
        (
            read["VersionId"].as_str(),
            read["SecretString"].as_str()
        ),
        (Some(CURRENT_TOKEN), Some(INITIAL_VALUE)),
        "expected the version and the value the secret served before the rotation, got {read}"
    );
}
