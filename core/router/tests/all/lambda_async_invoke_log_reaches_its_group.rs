// SPDX-License-Identifier: CC0-1.0

//! What a client reading a function's own Amazon CloudWatch Logs group SEES of an invocation
//! it made asynchronously.
//!
//! RULE under test: **AWS Lambda delivers the log of every run it makes of a function to that
//! function's log group — the group its `LoggingConfig.LogGroup` names, else the default
//! `/aws/lambda/<function name>` — whatever invocation type asked for the run, so a run an
//! `Event` invocation queued leaves the same records behind as one a `RequestResponse`
//! invocation ran: the `START RequestId:`/`END RequestId:`/`REPORT RequestId:` framing the
//! platform writes and every line the handler itself wrote, each readable through
//! `FilterLogEvents` on that group, including by a `filterPattern` quoting a term the handler
//! printed.**
//!
//! An asynchronous invocation's answer says nothing about the run ("the API response only
//! includes a status code"), so the function's log group is the ONLY place a client can
//! observe that the run happened at all: `aws logs tail`, a metric filter over handler output
//! and every "invoke, then look for the marker" diagnosis loop read it and nothing else. A run
//! whose log never reaches the group leaves such a client unable to tell a function that ran
//! from one that was never invoked.
//!
//! Four crates have to agree for that to hold — CloudFormation provisions the function (and,
//! in one leg, IAM holds the execution role a client wrote itself), AWS Lambda queues the
//! event, runs the handler and delivers the records, IAM answers for the role the delivery is
//! judged against, and Amazon CloudWatch Logs holds the group they land in — so the suite
//! lives here rather than in any one crate.
//! `crate::lambda_log_records_delivered_under_the_roles_logs_grant` pins the FORM of each
//! delivered record for a synchronous run, and
//! `crate::cloudformation_lambda_function_log_destination` WHERE a deployed function's records
//! land (and owns the wire harness and group readers this suite reuses); what only this suite
//! reaches is the invocation TYPE that asked for the run.
//!
//! The rows differ along what the rule generalises over rather than restating one template:
//! the invocation type that asked for the run (`Event`, the asynchronous one; the explicit
//! `RequestResponse`; and the default, which the call states no type at all for), and how many
//! runs of the function the row asks for, so a log delivered for the first run only fails a
//! row. The second test varies the OTHER thing the delivery depends on — where the execution
//! role's logs grant comes from: a role a client created through IAM's own API and granted by
//! ATTACHING AWS's published policy for the purpose, named by its ARN, so nothing of the
//! function's stack states what the role may do; it also reads the group back by NAME, because
//! a run whose log reached nothing leaves a client that looks the group up with a refusal.
//!
//! No expected value here comes from a recording: each marker is this suite's own and every
//! asserted record is built from it.

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
// The ONE assembled emulator, wire harness and group readers this test crate owns for a
// deployed function's log, rather than second copies of them.
use crate::cloudformation_lambda_function_log_destination::{
    cloudformation, emulator, groups_named, logs, trust_policy, wait_until_active, Answered,
    CREATE_COMPLETE, DEFAULT_GROUP_PREFIX, FUNCTION_TIMEOUT,
};
use crate::query_wire::outputs_of;
// IAM's own awsQuery caller, so the role the second test provisions is written the way a client
// writes one rather than through a second harness.
use crate::sns_lambda_subscription_permission_boundary::{iam, ACCOUNT};

/// The Region every request here is signed for — the one the shared stack reader polls in.
const REGION: &str = "us-east-1";

/// The logical ids the templates below declare.
const ROLE_ID: &str = "ExecutionRole";
const FUNCTION_ID: &str = "Asynchronous";

/// The ARN of the policy AWS publishes for a function that writes its own logs, which is what
/// an execution role carries for the purpose.
const BASIC_EXECUTION_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole";

/// The header the invocation type is carried in: `InvocationType` is bound to
/// `X-Amz-Invocation-Type` (`smithy_query(query="operation",operation="Invoke")`). HTTP header
/// names are case-insensitive, which is how an SDK writes this one.
const INVOCATION_TYPE_HEADER: &str = "x-amz-invocation-type";

/// The header AWS Lambda reports a failed handler under: `FunctionError` is bound to
/// `X-Amz-Function-Error`, so a run whose handler returned normally carries no such header.
const FUNCTION_ERROR_HEADER: &str = "x-amz-function-error";

/// The status an accepted asynchronous invocation is answered with — "202 Accepted" — and the
/// one every other call here answers.
const ACCEPTED_STATUS: u16 = 202;
const OK_STATUS: u16 = 200;

/// The fixed text of the frames the platform writes around every run it finishes.
const START_PREFIX: &str = "START RequestId: ";
/// See [`START_PREFIX`].
const END_PREFIX: &str = "END RequestId: ";
/// See [`START_PREFIX`].
const REPORT_PREFIX: &str = "REPORT RequestId: ";
/// See [`START_PREFIX`]: the frames of one run, the least the platform itself leaves behind.
const FRAMES: [&str; 3] = [START_PREFIX, END_PREFIX, REPORT_PREFIX];

/// The field names the `REPORT` record states what the run spent under. The values are the
/// run's own, so nothing here asserts on them.
const REPORT_FIELDS: [&str; 4] = [
    "Duration:",
    "Billed Duration:",
    "Memory Size:",
    "Max Memory Used:",
];

/// The prefix the handler prints its marker behind, so one run's line is told from any other
/// write in the group.
const PRINTED_PREFIX: &str = "EVENT_RECEIVED::";

/// The handler every leg deploys: it prints the marker the event carried, the way a client's
/// own diagnosis handler does, and answers it.
const HANDLER_SOURCE: &str = r#"
import json


def handler(event, context):
    print("EVENT_RECEIVED::" + json.dumps(event))
    return {"received": event["detail"]["marker"]}
"#;

/// How long to wait between looks at something a service owes, and how many looks to take. The
/// budget is far longer than any span these services choose, so a loaded host is slower without
/// the rule being any different; nothing here asserts on elapsed time.
const POLL: std::time::Duration = std::time::Duration::from_millis(50);
const POLLS: usize = 600;

/// One row: the invocation type the call carries (or none at all), the status it is answered
/// with, and how many runs the row asks for.
struct Row {
    case: &'static str,
    /// The `X-Amz-Invocation-Type` the call states, or `None` where it states none and the
    /// operation's own default applies ("by default, Lambda invokes your function
    /// synchronously").
    invocation_type: Option<&'static str>,
    /// The HTTP status that call is answered with.
    status: u16,
    /// How many times the row invokes the function, each with a marker of its own.
    runs: usize,
}

const ROWS: &[Row] = &[
    Row {
        case: "an asynchronous invocation",
        invocation_type: Some("Event"),
        status: ACCEPTED_STATUS,
        runs: 1,
    },
    Row {
        case: "two asynchronous invocations of the same function",
        invocation_type: Some("Event"),
        status: ACCEPTED_STATUS,
        runs: 2,
    },
    Row {
        case: "an invocation the caller waited for",
        invocation_type: Some("RequestResponse"),
        status: OK_STATUS,
        runs: 1,
    },
    Row {
        case: "an invocation stating no type at all",
        invocation_type: None,
        status: OK_STATUS,
        runs: 1,
    },
];

/// The template a row deploys: its execution role, granted AWS's own policy for a function that
/// writes its own logs, and the function that runs as it.
fn template(function: &str) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            ROLE_ID: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": trust_policy(),
                    "ManagedPolicyArns": [BASIC_EXECUTION_POLICY]
                }
            },
            FUNCTION_ID: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function,
                    "Role": { "Fn::GetAtt": [ROLE_ID, "Arn"] },
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Timeout": FUNCTION_TIMEOUT,
                    "MemorySize": 128,
                    "Code": { "ZipFile": HANDLER_SOURCE }
                }
            }
        },
        "Outputs": {
            "FunctionName": { "Value": { "Ref": FUNCTION_ID } }
        }
    })
    .to_string()
}

/// The same function, running as a role `role_arn` names — one a client provisioned through
/// IAM's own API, so nothing of this stack states what the role may do.
fn template_naming(function: &str, role_arn: &str) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            FUNCTION_ID: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function,
                    "Role": role_arn,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Timeout": FUNCTION_TIMEOUT,
                    "MemorySize": 128,
                    "Code": { "ZipFile": HANDLER_SOURCE }
                }
            }
        },
        "Outputs": {
            "FunctionName": { "Value": { "Ref": FUNCTION_ID } }
        }
    })
    .to_string()
}

/// `FilterLogEvents` on `group` for the events a `filterPattern` admits, as it was answered: a
/// group nothing has written to yet does not exist, which that service answers as a refusal
/// rather than as an empty list.
async fn filtered_for(router: &MultiServiceRouter, group: &str, pattern: &str) -> Answered {
    logs(
        router,
        "FilterLogEvents",
        json!({ "logGroupName": group, "filterPattern": pattern }),
    )
    .await
}

/// The messages `group` holds for `pattern` right now, as a client polling for a printed marker
/// reads them.
async fn matching(router: &MultiServiceRouter, group: &str, pattern: &str) -> Vec<String> {
    let answered = filtered_for(router, group, pattern).await;
    if answered.status != OK_STATUS {
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

/// The messages `group` holds for `pattern`, polled until there is at least one or the polls run
/// out. The delivery of a run's log is not part of an invocation's answer — and an asynchronous
/// invocation's answer is given before the run — so this waits for the CONDITION rather than for
/// a span.
async fn matching_eventually(
    router: &MultiServiceRouter,
    group: &str,
    pattern: &str,
) -> Vec<String> {
    let mut held = matching(router, group, pattern).await;
    for _ in 0..POLLS {
        if !held.is_empty() {
            return held;
        }
        tokio::time::sleep(POLL).await;
        held = matching(router, group, pattern).await;
    }
    held
}

/// Invoke `function` with `marker`, under `row`'s invocation type, and assert the call itself was
/// answered the status that type promises. Whatever became of the run is not part of this: an
/// asynchronous invocation's answer is given before the handler runs.
async fn invoked(router: &MultiServiceRouter, row: &Row, function: &str, marker: &str) {
    let case = row.case;
    let path = format!("/2015-03-31/functions/{function}/invocations");
    let payload = json!({ "detail": { "marker": marker } });
    let headers: Vec<(&str, &str)> = match row.invocation_type {
        Some(kind) => vec![(INVOCATION_TYPE_HEADER, kind)],
        None => Vec::new(),
    };
    let (status, function_error, body) = invoke_call(router, &path, &payload, &headers).await;
    assert_eq!(
        status, row.status,
        "case {case:?}: expected HTTP {} from Invoke of {function:?}, got {status} with {body:?}",
        row.status,
    );
    assert_eq!(
        function_error, None,
        "case {case:?}: expected no FunctionError from Invoke of {function:?}, got \
         {function_error:?}"
    );
}

/// One `Invoke` call as it arrives on the wire, carrying the invocation type a row asks for: its
/// status, the `X-Amz-Function-Error` it was answered with where it was answered one at all, and
/// its body.
async fn invoke_call(
    router: &MultiServiceRouter,
    path: &str,
    payload: &Value,
    headers: &[(&str, &str)],
) -> (u16, Option<String>, String) {
    let host = format!("lambda.{REGION}.amazonaws.com");
    let mut builder = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("content-type", "application/json")
        // The suite exercises the services, not SigV4: the credential scope names the target
        // service so the router can route the call, and the signature itself is not checked.
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/lambda/aws4_request, \
                 SignedHeaders=host, Signature=not-verified"
            ),
        );
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(Bytes::from(payload.to_string()))
        .expect("a well-formed request");
    let (parts, body) = router.dispatch(request).await.into_parts();
    let function_error = parts
        .headers
        .get(FUNCTION_ERROR_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    (
        parts.status.as_u16(),
        function_error,
        String::from_utf8_lossy(&body).to_string(),
    )
}

/// Assert that `group` holds the log of the run `marker` named: the handler's own printed line,
/// found by a `filterPattern` QUOTING the marker — which is how a client polls for it — and the
/// platform's three frames of that run, the `REPORT` one stating what the run spent.
async fn assert_run_is_logged(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    marker: &str,
) {
    let quoted = format!("\"{marker}\"");
    let found = matching_eventually(router, group, &quoted).await;
    assert!(
        !found.is_empty(),
        "case {case:?}: expected {group:?} to hold an event matching {quoted:?} for the run of \
         {marker:?}, got {found:?}"
    );
    assert!(
        found
            .iter()
            .any(|message| message.contains(PRINTED_PREFIX) && message.contains(marker)),
        "case {case:?}: expected the handler's own printed line for {marker:?}, got {found:?}"
    );
    for frame in FRAMES {
        let framed = matching_eventually(router, group, &format!("\"{frame}\"")).await;
        assert!(
            !framed.is_empty(),
            "case {case:?}: expected a record starting {frame:?} in {group:?} for the run of \
             {marker:?}, got {framed:?}"
        );
    }
    let reports = matching_eventually(router, group, &format!("\"{REPORT_PREFIX}\"")).await;
    for field in REPORT_FIELDS {
        assert!(
            reports.iter().any(|message| message.contains(field)),
            "case {case:?}: expected the REPORT record of {marker:?} to state {field:?}, got \
             {reports:?}"
        );
    }
}

/// RULE: a run an `Event` invocation queued leaves the same log behind as one a caller waited
/// for — the platform's framing and the handler's own line, in the function's own group — so a
/// client polling that group for a printed marker finds it.
#[tokio::test]
async fn an_asynchronous_invocations_run_is_logged_to_the_functions_own_group() {
    // ONE emulator for every row: each row deploys a stack of its own, so the function and the
    // group a row reads are still that row's alone.
    let router = emulator();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let function = format!("xsvc-async-log-{index}");
        let stack = format!("xsvc-async-log-{index}");
        let body = template(&function);
        let created = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", &stack),
                ("TemplateBody", &body),
                // The template declares an IAM role, which is acknowledged with CAPABILITY_IAM.
                ("Capabilities.member.1", "CAPABILITY_IAM"),
            ],
        )
        .await;
        assert_eq!(
            created.status, OK_STATUS,
            "case {case:?}: expected HTTP 200 from CreateStack, got {} with {:?}",
            created.status,
            created.text()
        );
        let (reached, described) = wait_for_stack_status(&router, &stack, CREATE_COMPLETE).await;
        assert_eq!(
            reached, CREATE_COMPLETE,
            "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {described}"
        );
        let published = outputs_of(&described)
            .get("FunctionName")
            .cloned()
            .unwrap_or_else(|| {
                panic!("case {case:?}: the stack published no function name in {described}")
            });
        assert_eq!(
            published, function,
            "case {case:?}: expected the stack to publish the declared function name"
        );
        // The function has to be invocable before any of this can be read: "During this time,
        // you can't invoke or modify the function" (`CreateFunction`). The span is that
        // service's own, so the wait is on that CONDITION.
        wait_until_active(&router, case, &function).await;
        let group = format!("{DEFAULT_GROUP_PREFIX}{function}");

        for run in 0..row.runs {
            let marker = format!("{function}-run-{run}");
            invoked(&router, row, &function, &marker).await;
            assert_run_is_logged(&router, case, &group, &marker).await;
        }
    }
}

/// The role, stack and function of the leg below, named for it so it stands beside the rows
/// above rather than over them.
const ATTACHED_ROLE: &str = "xsvc-async-log-attached-exec";
const ATTACHED_STACK: &str = "xsvc-async-log-attached";
const ATTACHED_FUNCTION: &str = "xsvc-async-log-attached";

/// RULE, over the OTHER way an execution role comes to allow the delivery: the grant is AWS's
/// published policy ATTACHED to a role a client created through IAM itself, so an asynchronous
/// run of a function carrying that role is logged to its group just the same — and the group is
/// one a client holding its name can FIND.
#[tokio::test]
async fn an_asynchronous_run_of_a_function_whose_role_carries_the_attached_policy_is_logged() {
    let case = "a role granted by AWS's own attached policy";
    let router = emulator();

    // The role, written the way a client writes one: created with the trust policy AWS Lambda
    // is handed it under, then granted by ATTACHING the policy AWS publishes for the purpose.
    iam(
        &router,
        "CreateRole",
        &[
            ("RoleName", ATTACHED_ROLE),
            ("AssumeRolePolicyDocument", &trust_policy().to_string()),
        ],
    )
    .await;
    iam(
        &router,
        "AttachRolePolicy",
        &[
            ("RoleName", ATTACHED_ROLE),
            ("PolicyArn", BASIC_EXECUTION_POLICY),
        ],
    )
    .await;

    let role_arn = format!("arn:aws:iam::{ACCOUNT}:role/{ATTACHED_ROLE}");
    let body = template_naming(ATTACHED_FUNCTION, &role_arn);
    let created = cloudformation(
        &router,
        "CreateStack",
        &[("StackName", ATTACHED_STACK), ("TemplateBody", &body)],
    )
    .await;
    assert_eq!(
        created.status, OK_STATUS,
        "case {case:?}: expected HTTP 200 from CreateStack, got {} with {:?}",
        created.status,
        created.text()
    );
    let (reached, described) = wait_for_stack_status(&router, ATTACHED_STACK, CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {described}"
    );
    wait_until_active(&router, case, ATTACHED_FUNCTION).await;

    let group = format!("{DEFAULT_GROUP_PREFIX}{ATTACHED_FUNCTION}");
    let marker = format!("{ATTACHED_FUNCTION}-marker");
    let row = Row {
        case,
        invocation_type: Some("Event"),
        status: ACCEPTED_STATUS,
        runs: 1,
    };
    invoked(&router, &row, ATTACHED_FUNCTION, &marker).await;
    assert_run_is_logged(&router, case, &group, &marker).await;

    // The group itself is one a client holding its name can FIND: a run whose log reached
    // nothing leaves such a client with a refusal instead.
    let named = groups_named(&router, case, &group).await;
    assert_eq!(
        named.len(),
        1,
        "case {case:?}: expected exactly the group {group:?} to be resolvable by name, got \
         {named:?}"
    );
}
