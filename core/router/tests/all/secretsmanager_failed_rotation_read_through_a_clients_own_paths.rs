// SPDX-License-Identifier: CC0-1.0

//! What a CLIENT reads after a rotation whose function could not complete a step: the
//! function's own log group, and the value the secret still serves.
//!
//! The rule under test: **a rotation stops at the step its rotation function did not complete,
//! and the two paths a client watches it through report exactly that. The function's default
//! log group — "a default log group named `/aws/lambda/<function name>`"
//! (`LoggingConfig$LogGroup`) — holds the record each invocation that RAN wrote and none for a
//! step that never ran, and `FilterLogEvents` selects those records by the term the handler
//! printed however the client spells that term in the filter-and-pattern syntax (a bare word, a
//! `"…"` string, a `?` alternative, a `%…%` expression, or no pattern at all), while a term no
//! record carries selects nothing; and `GetSecretValue` asked for no version at all keeps
//! serving the version the secret served before the rotation, for as long as the client keeps
//! looking. The same source with only its failure flag cleared completes the protocol and
//! promotes the candidate.**
//!
//! That is the safety property of rotation as a CLIENT can establish it: an untested credential
//! is never made current, the client can tell WHICH step its function failed at, and a rotation
//! that stopped safely is told from one that never started.
//!
//! The sibling suite `secretsmanager_rotation_stops_at_the_step_its_function_raises_in` spans
//! WHICH step the function stops at, reading the group with no filter pattern. This one holds
//! one failing step and spans what a client's own READING of that evidence generalises over: the
//! form the `filterPattern` selecting the handler's records is written in, which is the client's
//! to choose and must not decide whether the records are found. The pagination of that read is
//! asserted too — a continuation reports what is LEFT of the page rather than the page again —
//! because a client that collects every page must not be handed the same record twice.
//!
//! Four crates have to agree for the rule to hold — CloudFormation deploys the two rotation
//! functions and the role they run as, IAM answers for that role and for the grant Secrets
//! Manager invokes them under, Lambda runs each invocation and delivers its output, CloudWatch
//! Logs holds and filters it, Secrets Manager drives the protocol — so the suite lives here.
//! Every wait below looks for the STATE it waits on, and its ceiling is there only so the suite
//! cannot hang.

use std::collections::BTreeMap;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode, opt_element, outputs_of};
use crate::secretsmanager_rotation_function_is_tested_and_validated::{
    build_router, described, secretsmanager_ok as served_in, stages_of, token, CURRENT_STAGE,
    POLL, POLLS, REGION, ROTATION_STEPS,
};

/// The staging label a rotation's candidate carries until a `finishSecret` step promotes it,
/// and the one the version a promotion replaced carries afterwards.
const PENDING_STAGE: &str = "AWSPENDING";
const PREVIOUS_STAGE: &str = "AWSPREVIOUS";

/// The value the secret is created with: the failing arm must still serve it, and the arm that
/// completes must serve it with the suffix its function appended.
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

/// The term every invocation of the rotation function prints before it does anything else, and
/// which a client selects those records by. It is the FUNCTION's own output, not a record the
/// platform frames, and it is spelled in the word characters the filter-and-pattern syntax
/// admits in a bare term so that every row of the table below can state it.
const STEP_MARKER: &str = "ROTATION_STEP";

/// The environment variable each function reads the step it cannot complete from: the two
/// functions deploy ONE source and differ only in this value.
const FAIL_AT_VARIABLE: &str = "FAIL_AT";

/// The step the failing function cannot complete, and so the step the rotation stops at.
const FAILING_STEP: &str = "testSecret";

/// The function names, and so the log groups, of the two twins: one raising in [`FAILING_STEP`]
/// and one completing every step.
const FAILING_FUNCTION: &str = "clientreadrotatorfail";
const PASSING_FUNCTION: &str = "clientreadrotatorpass";

/// How many looks the failing arm's state is watched over. The window is counted in LOOKS
/// rather than in time, so it stretches with the host: every look is a round trip through the
/// router, and what it asserts is that nothing MOVES, so a slow host can only make it miss a
/// violation rather than invent one.
const QUIET_LOOKS: usize = 12;

/// The rotation function both twins are deployed from, written the way the AWS rotation
/// templates are: it PRINTS the step it was invoked with first of all, then does that step's own
/// work through Secrets Manager's own operations — `createSecret` stages a candidate under
/// `AWSPENDING`, `testSecret` reads it back, `finishSecret` moves `AWSCURRENT` onto it — and
/// raises on the step [`FAIL_AT_VARIABLE`] names.
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
    print("ROTATION_STEP %s" % step)
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

/// One row of the table: how a client spells the `filterPattern` that selects the handler's step
/// records, and which steps that pattern must then select.
struct Row {
    case: &'static str,
    /// The `filterPattern` the client sends, `""` for a read that sends none.
    pattern: &'static str,
    /// Whether the pattern selects the records carrying [`STEP_MARKER`] at all: every form of
    /// the marker does, and the one naming a term no record carries selects nothing.
    selects_the_steps: bool,
}

/// The forms a term is written in, from the CloudWatch Logs FILTER AND PATTERN syntax an
/// unstructured `filterPattern` is read in: a bare word, a `"…"` string, a `?` alternative and a
/// `%…%` regular expression, beside the empty pattern that selects every event. The last row is
/// the other side of the same rule: a term no record carries selects nothing, which is how a
/// client establishes that the step it names never ran.
const ROWS: &[Row] = &[
    Row {
        case: "no filter pattern at all",
        pattern: "",
        selects_the_steps: true,
    },
    Row {
        case: "the marker as a bare term",
        pattern: STEP_MARKER,
        selects_the_steps: true,
    },
    Row {
        case: "the marker as a quoted string",
        pattern: "\"ROTATION_STEP\"",
        selects_the_steps: true,
    },
    Row {
        case: "the marker as one of two alternatives",
        pattern: "?ROTATION_STEP ?NO_SUCH_TERM",
        selects_the_steps: true,
    },
    Row {
        case: "the marker as a regular expression",
        pattern: "%ROTATION_[A-Z]+%",
        selects_the_steps: true,
    },
    Row {
        case: "a term no record of the group carries",
        pattern: "\"ROTATION_STEP_THAT_NEVER_RAN\"",
        selects_the_steps: false,
    },
];

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
/// it is exactly what a client polling for a record that has not come yet sees.
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

/// The log group a function's invocations are delivered to.
fn log_group(function: &str) -> String {
    format!("{DEFAULT_GROUP_PREFIX}{function}")
}

/// One `FilterLogEvents` page of `group`, selected by `pattern` (sent only when it is not
/// empty) and continued from `token` when there is one: the messages it reported and the
/// continuation token it answered with.
async fn filtered_page(
    router: &MultiServiceRouter,
    group: &str,
    pattern: &str,
    token: Option<&str>,
) -> (Vec<String>, Option<String>) {
    let mut request = json!({ "logGroupName": group });
    if !pattern.is_empty() {
        request["filterPattern"] = json!(pattern);
    }
    if let Some(token) = token {
        request["nextToken"] = json!(token);
    }
    let (status, answered) = logs(router, "FilterLogEvents", &request).await;
    if status != 200 {
        return (Vec::new(), None);
    }
    let messages = answered["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|event| event["message"].as_str().map(str::to_string))
        .collect();
    let next = answered["nextToken"].as_str().map(str::to_string);
    (messages, next)
}

/// The steps `messages` record, in the order the invocations wrote them: the tail of every
/// message carrying [`STEP_MARKER`]. A message the handler did not write carries none.
fn steps_in(messages: &[String]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|message| message.split(STEP_MARKER).nth(1))
        .map(|tail| tail.trim().to_string())
        .collect()
}

/// Every page of that read, collected the way a client collects them: follow the continuation
/// token until a page reports none, and answer the steps the whole read recorded.
async fn steps_logged(router: &MultiServiceRouter, group: &str, pattern: &str) -> Vec<String> {
    let mut steps = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..POLLS {
        let (messages, next) = filtered_page(router, group, pattern, token.as_deref()).await;
        steps.extend(steps_in(&messages));
        match next {
            Some(next) if !messages.is_empty() => token = Some(next),
            _ => return steps,
        }
    }
    steps
}

/// Poll `group` until it holds `wanted` step records, and answer the steps it then holds.
/// Nothing here sleeps on an invocation's behalf: the invocations are the service's own work,
/// and this looks for the record of them.
async fn wait_for_steps(router: &MultiServiceRouter, group: &str, wanted: usize) -> Vec<String> {
    let mut seen = Vec::new();
    for _ in 0..POLLS {
        seen = steps_logged(router, group, "").await;
        if seen.len() >= wanted {
            return seen;
        }
        tokio::time::sleep(POLL).await;
    }
    seen
}

/// Call one Secrets Manager operation in this suite's Region, requiring the 200 it answers.
async fn secretsmanager(
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

/// The template deploying both twins: ONE execution role that may read and write the secrets
/// and write its own logs, the two functions differing only in the step they raise on, and the
/// grant that lets Secrets Manager invoke each.
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
    for (function, fail_at) in [(FAILING_FUNCTION, FAILING_STEP), (PASSING_FUNCTION, "")] {
        let logical = format!("Function{function}");
        resources.insert(
            logical.clone(),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function,
                    "Runtime": "python3.12",
                    "Handler": "index.lambda_handler",
                    "Role": { "Fn::GetAtt": ["RotationRole", "Arn"] },
                    "Timeout": FUNCTION_TIMEOUT,
                    "Environment": { "Variables": { FAIL_AT_VARIABLE: fail_at } },
                    "Code": { "ZipFile": ROTATION_FUNCTION }
                }
            }),
        );
        resources.insert(
            format!("Permission{function}"),
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
            format!("Arn{function}"),
            json!({ "Value": { "Fn::GetAtt": [logical, "Arn"] } }),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "the two rotation function twins this suite's rotations are driven through",
        "Resources": resources,
        "Outputs": outputs,
    })
    .to_string()
}

/// Deploy that template, and answer each twin's function ARN by its name.
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
    [FAILING_FUNCTION, PASSING_FUNCTION]
        .into_iter()
        .map(|function| {
            let key = format!("Arn{function}");
            let arn = published
                .get(&key)
                .unwrap_or_else(|| panic!("setup: no {key} output in {}", last.1))
                .clone();
            (function.to_string(), arn)
        })
        .collect()
}

/// RULE: a rotation whose function raises a step leaves the credential alone, and a client
/// establishes that through the function's own log group — selected by the marker its handler
/// printed, however the client spells that term — and through a plain `GetSecretValue`; the twin
/// with the failure flag cleared completes the protocol and promotes the candidate.
#[tokio::test]
async fn a_failed_rotation_is_read_through_a_clients_own_paths() {
    let router = build_router();
    let functions = deployed_functions(&router, "clientreadrotationstack").await;
    let case = "a rotation function that raises in testSecret";

    let name = "client-read-credentials";
    let created = secretsmanager(
        &router,
        case,
        "CreateSecret",
        &json!({
            "Name": name,
            "ClientRequestToken": token("clientreadorig"),
            "SecretString": INITIAL_VALUE,
        }),
    )
    .await;
    let original = created["VersionId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no VersionId in {created}"))
        .to_string();

    let candidate = token("clientreadcand");
    let started = secretsmanager(
        &router,
        case,
        "RotateSecret",
        &json!({
            "SecretId": name,
            "ClientRequestToken": candidate,
            "RotationLambdaARN": functions[FAILING_FUNCTION],
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

    // The steps that RAN: every step up to and including the one the function raised in.
    let reached = ROTATION_STEPS
        .iter()
        .position(|step| *step == FAILING_STEP)
        .map_or(ROTATION_STEPS.len(), |at| at + 1);
    let expected_steps: Vec<String> = ROTATION_STEPS[..reached]
        .iter()
        .map(|step| (*step).to_string())
        .collect();
    let group = log_group(FAILING_FUNCTION);
    let steps = wait_for_steps(&router, &group, reached).await;
    assert_eq!(
        steps, expected_steps,
        "case {case:?}: expected the steps {expected_steps:?} once each, in that order, got \
         {steps:?} in {group}"
    );

    // THE TABLE: the form the client spells the marker in does not decide whether the records
    // are found, and a term no record carries finds none.
    for row in ROWS {
        let selected = steps_logged(&router, &group, row.pattern).await;
        let wanted = if row.selects_the_steps {
            expected_steps.clone()
        } else {
            Vec::new()
        };
        assert_eq!(
            selected, wanted,
            "case {:?}: expected the pattern {:?} over {group} to select {wanted:?}, got \
             {selected:?}",
            row.case, row.pattern
        );
    }

    // ... and the pagination of that read reports what is LEFT of it rather than the page
    // again: a client that follows the continuation token is handed no record twice.
    let (first_page, continuation) = filtered_page(&router, &group, STEP_MARKER, None).await;
    assert_eq!(
        steps_in(&first_page),
        expected_steps,
        "case {case:?}: expected the first page to hold {expected_steps:?}, got {first_page:?}"
    );
    if let Some(continuation) = continuation {
        let (continued, _) =
            filtered_page(&router, &group, STEP_MARKER, Some(&continuation)).await;
        assert_eq!(
            steps_in(&continued),
            Vec::<String>::new(),
            "case {case:?}: expected the continuation of a complete page to hold no further \
             step record, got {continued:?}"
        );
    }

    // What the secret is left as, for as long as the client keeps looking: the candidate the
    // function staged holds AWSPENDING, AWSCURRENT has not moved, the rotation did not finish,
    // and the step the function never reached never runs.
    let expected_stages = BTreeMap::from([
        (original.clone(), vec![CURRENT_STAGE.to_string()]),
        (candidate.clone(), vec![PENDING_STAGE.to_string()]),
    ]);
    for look in 0..QUIET_LOOKS {
        let reported = described(&router, case, name).await;
        assert_eq!(
            stages_of(&reported),
            expected_stages,
            "case {case:?}, look {look}: expected {expected_stages:?}, got {reported}"
        );
        assert!(
            reported.get("LastRotatedDate").is_none(),
            "case {case:?}, look {look}: expected no LastRotatedDate for a rotation that did \
             not finish, got {reported}"
        );
        // The path the client reads the credential by: no version and no staging label named,
        // which is the version the secret SERVES.
        let read = secretsmanager(
            &router,
            case,
            "GetSecretValue",
            &json!({ "SecretId": name }),
        )
        .await;
        assert_eq!(
            (read["VersionId"].as_str(), read["SecretString"].as_str()),
            (Some(original.as_str()), Some(INITIAL_VALUE)),
            "case {case:?}, look {look}: expected the version the secret served before the \
             rotation, got {read}"
        );
        let watched = steps_logged(&router, &group, STEP_MARKER).await;
        assert_eq!(
            watched, expected_steps,
            "case {case:?}, look {look}: expected the function to have been invoked once per \
             step it reached and no more, got {watched:?}"
        );
        tokio::time::sleep(POLL).await;
    }

    // THE POSITIVE CONTROL: the same source with only its failure flag cleared. The documented
    // recovery takes the outstanding rotation up again — "turns off automatic rotation, and if a
    // rotation is currently in progress, cancels the rotation" (`CancelRotateSecret`, Secrets
    // Manager's Smithy model), then a further `RotateSecret` — and this one completes.
    let control = "the same source with its failure flag cleared";
    secretsmanager(
        &router,
        control,
        "CancelRotateSecret",
        &json!({ "SecretId": name }),
    )
    .await;
    let resumed = secretsmanager(
        &router,
        control,
        "RotateSecret",
        &json!({
            "SecretId": name,
            "RotationLambdaARN": functions[PASSING_FUNCTION],
            "RotationRules": { "AutomaticallyAfterDays": 30 },
            "RotateImmediately": true,
        }),
    )
    .await;
    let promoted = resumed["VersionId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {control:?}: no VersionId in {resumed}"))
        .to_string();

    // The twin runs every step, so its own log group holds all four of them...
    let passing_group = log_group(PASSING_FUNCTION);
    let steps = wait_for_steps(&router, &passing_group, ROTATION_STEPS.len()).await;
    assert_eq!(
        steps,
        ROTATION_STEPS
            .iter()
            .map(|step| (*step).to_string())
            .collect::<Vec<String>>(),
        "case {control:?}: expected every step once each in {passing_group}, got {steps:?}"
    );

    // ... and the rotation it drove promoted the candidate: AWSCURRENT on it, AWSPREVIOUS on
    // the version it replaced, the value its own `createSecret` staged, and the rotation
    // stamped as finished.
    let mut reported = Value::Null;
    for _ in 0..POLLS {
        reported = described(&router, control, name).await;
        if staged_versions(&reported, CURRENT_STAGE) == vec![promoted.clone()] {
            break;
        }
        tokio::time::sleep(POLL).await;
    }
    assert_eq!(
        staged_versions(&reported, CURRENT_STAGE),
        vec![promoted.clone()],
        "case {control:?}: expected {CURRENT_STAGE} on the candidate {promoted:?}, got {reported}"
    );
    assert_eq!(
        staged_versions(&reported, PREVIOUS_STAGE),
        vec![original.clone()],
        "case {control:?}: expected {PREVIOUS_STAGE} on the version the rotation replaced, got \
         {reported}"
    );
    assert!(
        reported.get("LastRotatedDate").is_some(),
        "case {control:?}: expected LastRotatedDate once the rotation finished, got {reported}"
    );
    let read = secretsmanager(
        &router,
        control,
        "GetSecretValue",
        &json!({ "SecretId": name }),
    )
    .await;
    let expected_value = format!("{INITIAL_VALUE}{ROTATED_SUFFIX}");
    assert_eq!(
        (read["VersionId"].as_str(), read["SecretString"].as_str()),
        (Some(promoted.as_str()), Some(expected_value.as_str())),
        "case {control:?}: expected the value the rotation staged, got {read}"
    );
}
