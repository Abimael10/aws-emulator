// SPDX-License-Identifier: CC0-1.0

//! A Lambda-backed custom resource whose provider answers the `Create` with `FAILED`, and
//! what the stack behind it then does.
//!
//! RULE under test: **a custom resource whose provider answers a request with `Status`
//! `FAILED` fails that resource with the provider's own `Reason` — carried on the resource's
//! own stack event — which fails the stack operation and rolls it back; the rollback sends the
//! provider a `Delete` request for the resource the answer named and takes the resources that
//! custom resource depends on down only once that `Delete` is answered, and the log the run
//! answering it wrote is in the provider's log group — the lines of a run that already
//! happened are not lost because the rollback goes on to remove the role the function ran as
//! — so the stack reaches a terminal status in bounded time and a later `DeleteStack`
//! converges to `DELETE_COMPLETE` with none of the resources the stack made left behind.**
//!
//! Three crates have to agree for that to hold — CloudFormation sends the provider request
//! and holds the resource until it is answered, AWS Lambda runs the provider and records what
//! it printed, and Amazon CloudWatch Logs holds those records — so the suite lives here: the
//! `Delete` request the rollback sends is only observable where the provider's own log is
//! readable.
//!
//! The rows differ along what the rule generalises over: how the custom resource's type is
//! spelled (the generic `AWS::CloudFormation::CustomResource` and an author's own
//! `Custom::<Name>`), the `Reason` the provider states (plain text, and text carrying the
//! punctuation a diagnostic sentence has), and how the provider sends its answer (the
//! standard library's HTTP client, and the response module CloudFormation supplies to inline
//! code).

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode, members_of, opt_element};

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The statuses this suite waits for.
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";
const DELETE_COMPLETE: &str = "DELETE_COMPLETE";
const CREATE_FAILED: &str = "CREATE_FAILED";

/// How long a condition the clock owes is waited for, and how often it is looked at. The wait
/// is on the CONDITION: nothing here sleeps a fixed span on a service's behalf.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
const LOG_DEADLINE: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(50);

/// The logical id of the custom resource every row declares.
const CUSTOM: &str = "Probe";

/// What the provider prints for each request it is sent, so the log reader can tell the
/// `Create` it failed from the `Delete` the rollback owes.
const SAW_PREFIX: &str = "provider saw ";

/// A provider answering with the standard library's own HTTP client: it fails the `Create`
/// with the `Reason` its own properties carry and carries every other request out.
const ANSWERS_WITH_URLLIB: &str = r#"
import json
import urllib.request


def handler(event, context):
    request_type = event["RequestType"]
    print("provider saw %s" % request_type)
    physical = event.get("PhysicalResourceId") or "probe-physical-id"
    if request_type == "Create":
        answer = {
            "Status": "FAILED",
            "Reason": event["ResourceProperties"]["FailureReason"],
            "PhysicalResourceId": physical,
        }
    else:
        answer = {"Status": "SUCCESS", "PhysicalResourceId": physical}
    body = json.dumps(answer).encode("utf-8")
    request = urllib.request.Request(
        event["ResponseURL"],
        data=body,
        headers={"content-type": ""},
        method="PUT",
    )
    with urllib.request.urlopen(request) as answered:
        answered.read()
    return {}
"#;

/// The same provider, answering through the response module CloudFormation supplies to inline
/// code — the documented way an inline provider answers.
const ANSWERS_WITH_THE_RESPONSE_MODULE: &str = r#"
import cfnresponse


def handler(event, context):
    request_type = event["RequestType"]
    print("provider saw %s" % request_type)
    physical = event.get("PhysicalResourceId") or "probe-physical-id"
    if request_type == "Create":
        cfnresponse.send(
            event,
            context,
            cfnresponse.FAILED,
            {},
            physicalResourceId=physical,
            reason=event["ResourceProperties"]["FailureReason"],
        )
    else:
        cfnresponse.send(
            event, context, cfnresponse.SUCCESS, {}, physicalResourceId=physical
        )
"#;

/// One answered request: its status and its body verbatim.
struct Answered {
    status: u16,
    body: Bytes,
}

impl Answered {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// The emulator as it is ASSEMBLED, on one engine: the provider CloudFormation invokes is the
/// function this suite's own stack created, and the log it writes is the one read back here.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: every call carries a credential scope so
    // the router can name the target service, and the signature itself is not under test.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Dispatch one request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answered {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .dispatch(builder.body(body).expect("a well-formed request"))
        .await;
    let (parts, body) = response.into_parts();
    Answered {
        status: parts.status.as_u16(),
        body,
    }
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> Answered {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    dispatch(
        router,
        "POST",
        &format!("cloudformation.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization("cloudformation")),
        ],
        Bytes::from(body),
    )
    .await
}

/// One AWS Lambda call over its `restJson1` wire.
async fn lambda(router: &MultiServiceRouter, method: &str, path: &str) -> Answered {
    dispatch(
        router,
        method,
        &format!("lambda.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("lambda")),
        ],
        Bytes::new(),
    )
    .await
}

/// One Amazon CloudWatch Logs call over its `awsJson1_1` wire.
async fn logs(router: &MultiServiceRouter, operation: &str, body: Value) -> Answered {
    dispatch(
        router,
        "POST",
        &format!("logs.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("authorization", &authorization("logs")),
            ("x-amz-target", &format!("Logs_20140328.{operation}")),
        ],
        Bytes::from(body.to_string()),
    )
    .await
}

/// What the stack reports now: the `StackStatus` it carries, the answer it was read from, and
/// whether CloudFormation still holds a stack under that name at all — a `DescribeStacks` it
/// refuses is the answer for a stack whose deletion is through.
async fn stack_status(router: &MultiServiceRouter, stack: &str) -> (String, String, bool) {
    let answered = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
    let body = answered.text();
    (
        opt_element(&body, "StackStatus").unwrap_or_default(),
        body,
        answered.status == 200,
    )
}

/// Poll `stack` until it reports a status that is not `*_IN_PROGRESS` — or until
/// CloudFormation no longer holds it, which is where a completed deletion leaves it — and
/// answer the last status it reported with the answer that status was read from.
async fn settled_status(router: &MultiServiceRouter, stack: &str) -> (String, String) {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (reported, body, held) = stack_status(router, stack).await;
        let settled = !held || (!reported.is_empty() && !reported.ends_with("_IN_PROGRESS"));
        if settled || Instant::now() >= giving_up_at {
            return (reported, body);
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Every message the group `group` holds, with the lines each carries.
async fn logged_lines(router: &MultiServiceRouter, group: &str) -> Vec<String> {
    let answered = logs(router, "FilterLogEvents", json!({ "logGroupName": group })).await;
    if answered.status != 200 {
        return Vec::new();
    }
    answered
        .json()
        .get("events")
        .and_then(Value::as_array)
        .map(|events| {
            events
                .iter()
                .filter_map(|event| event.get("message").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Poll the group `group` until it holds at least `wanted` lines carrying `needle`, and answer
/// every line it then holds.
async fn awaited_lines(
    router: &MultiServiceRouter,
    group: &str,
    needle: &str,
    wanted: usize,
) -> Vec<String> {
    let giving_up_at = Instant::now() + LOG_DEADLINE;
    loop {
        let held = logged_lines(router, group).await;
        let seen = held.iter().filter(|line| line.contains(needle)).count();
        if seen >= wanted || Instant::now() >= giving_up_at {
            return held;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// One row: how the custom resource's type is spelled, what `Reason` its provider states, and
/// which provider source answers for it.
struct Row {
    case: &'static str,
    stack: &'static str,
    function: &'static str,
    resource_type: &'static str,
    reason: &'static str,
    source: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "the generic custom resource type, answered with the standard library's client",
        stack: "failed-custom-resource-generic",
        function: "failed-custom-resource-generic-provider",
        resource_type: "AWS::CloudFormation::CustomResource",
        reason: "the provider refused to make the thing",
        source: ANSWERS_WITH_URLLIB,
    },
    Row {
        case: "an author's own custom resource type, answered through the supplied module",
        stack: "failed-custom-resource-authored",
        function: "failed-custom-resource-authored-provider",
        resource_type: "Custom::Provisioner",
        reason: "Error: the upstream call answered 400 (AccessDenied). See the log stream.",
        source: ANSWERS_WITH_THE_RESPONSE_MODULE,
    },
];

/// The template one row deploys: the execution role the provider assumes, the provider
/// function under a name of the row's own, and the custom resource that function backs.
fn template(row: &Row) -> String {
    json!({
        "Resources": {
            "Role": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Action": "sts:AssumeRole",
                            "Effect": "Allow",
                            "Principal": {"Service": "lambda.amazonaws.com"},
                        }],
                    },
                    // The grant a provider function needs to leave its own log behind, which
                    // is what makes the request it was sent readable at all.
                    "Policies": [{
                        "PolicyName": "write-its-own-log",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": "logs:*",
                                "Resource": "*",
                            }],
                        },
                    }],
                },
            },
            "Provider": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": row.function,
                    "Code": {"ZipFile": row.source},
                    "Handler": "index.handler",
                    "Role": {"Fn::GetAtt": ["Role", "Arn"]},
                    "Runtime": "python3.12",
                    "Timeout": 60,
                },
            },
            CUSTOM: {
                "Type": row.resource_type,
                "Properties": {
                    "ServiceToken": {"Fn::GetAtt": ["Provider", "Arn"]},
                    "FailureReason": row.reason,
                },
            },
        },
    })
    .to_string()
}

/// Every stack event of `stack` for the logical resource `logical_id`, as the pairs of status
/// and reason a client reads.
///
/// `pub(crate)` with its caller below: a sibling suite that reads what a custom resource's own
/// events report drives THIS reader rather than a second copy of it.
pub(crate) async fn resource_events(
    router: &MultiServiceRouter,
    stack: &str,
    logical_id: &str,
) -> Vec<(String, String)> {
    let answered = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    let body = answered.text();
    members_of(&body)
        .into_iter()
        .filter(|member| {
            elements(member, "LogicalResourceId")
                .first()
                .is_some_and(|found| found == logical_id)
        })
        .map(|member| {
            (
                elements(member, "ResourceStatus")
                    .into_iter()
                    .next()
                    .unwrap_or_default(),
                elements(member, "ResourceStatusReason")
                    .into_iter()
                    .next()
                    .unwrap_or_default(),
            )
        })
        .collect()
}

#[tokio::test]
async fn a_failed_provider_answer_rolls_the_stack_back_and_deletes_its_resources() {
    for row in ROWS {
        let case = row.case;
        let router = emulator();
        let created = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", row.stack),
                ("TemplateBody", &template(row)),
                ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
            ],
        )
        .await;
        assert_eq!(
            created.status,
            200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {} with {:?}",
            created.status,
            created.text()
        );

        // 1. The stack settles, and settles ROLLED BACK: the provider's refusal fails the
        // operation rather than leaving it in flight.
        let (settled, described) = settled_status(&router, row.stack).await;
        assert_eq!(
            settled, ROLLBACK_COMPLETE,
            "case {case:?}: expected the stack to settle at {ROLLBACK_COMPLETE}, got \
             {settled:?} with {described}"
        );

        // 2. The resource's own event carries the provider's Reason.
        let events = resource_events(&router, row.stack, CUSTOM).await;
        let failed: Vec<&(String, String)> =
            events.iter().filter(|(status, _)| status == CREATE_FAILED).collect();
        assert!(
            !failed.is_empty(),
            "case {case:?}: expected a {CREATE_FAILED} event for {CUSTOM}, got {events:?}"
        );
        assert!(
            failed.iter().any(|(_, reason)| reason.contains(row.reason)),
            "case {case:?}: expected the {CREATE_FAILED} reason to carry the provider's own \
             reason {:?}, got {failed:?}",
            row.reason
        );

        // 3. The rollback sent the provider a Delete for the resource it failed to create, so
        // its log carries the Create it refused and the Delete that followed.
        let group = format!("/aws/lambda/{}", row.function);
        let delete_line = format!("{SAW_PREFIX}Delete");
        let held = awaited_lines(&router, &group, &delete_line, 1).await;
        assert!(
            held.iter().any(|line| line.contains(&format!("{SAW_PREFIX}Create"))),
            "case {case:?}: expected the provider's log {group:?} to carry the Create request \
             it was sent, got {held:?}"
        );
        assert!(
            held.iter().any(|line| line.contains(&delete_line)),
            "case {case:?}: expected the rollback to send the provider a Delete request and the \
             run that answered it to leave its log behind, so {group:?} carries \
             {delete_line:?}, got {held:?}"
        );

        // 4. A later DeleteStack converges, and leaves the function it made behind nowhere.
        let deleted =
            cloudformation(&router, "DeleteStack", &[("StackName", row.stack)]).await;
        assert_eq!(
            deleted.status,
            200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {} with {:?}",
            deleted.status,
            deleted.text()
        );
        let (settled, described) = settled_status(&router, row.stack).await;
        assert!(
            settled == DELETE_COMPLETE || settled.is_empty(),
            "case {case:?}: expected the deletion to converge to {DELETE_COMPLETE}, got \
             {settled:?} with {described}"
        );
        let reported = lambda(
            &router,
            "GET",
            &format!("/2015-03-31/functions/{}", row.function),
        )
        .await;
        assert_eq!(
            reported.status,
            404,
            "case {case:?}: expected the provider function to be left behind nowhere, got {} \
             with {:?}",
            reported.status,
            reported.text()
        );
    }
}
