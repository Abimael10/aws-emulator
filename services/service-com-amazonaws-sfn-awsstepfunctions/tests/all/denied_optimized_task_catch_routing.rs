// SPDX-License-Identifier: CC0-1.0

//! What a `Catch` written for a DENIED `Task` state matches, and what the failure it
//! routes carries — driven over both services' own wire protocols through the production
//! `MultiServiceRouter` with Step Functions and IAM mounted on ONE engine, the shape
//! `core/router/src/lib.rs` mounts them in, so the documents the call is judged against
//! are the ones IAM's own handlers wrote.
//!
//! RULE: for an OPTIMIZED service integration, the error name a `Task` state reports for a
//! refusal is the name Step Functions' own integration writes for the invoked service
//! followed by the SDK exception CLASS that answer resolves to — the class of the error
//! shape the invoked operation MODELS for an unauthorized call, and that service's BASE
//! exception class for an operation that models none — and never the wire error CODE the
//! answer named the class by. `ErrorEquals` matches that name exactly, so a `Catch` listing
//! it takes the denial over and the execution ends `SUCCEEDED` down the handler's branch,
//! while one listing any other spelling of the same refusal matches nothing and the
//! execution ends `FAILED`. The failure handed to that branch is the refusal as the client
//! an optimized integration is served through renders it: the invoked service's sentence,
//! then the semicolon-separated metadata naming that client's own name for the service, the
//! status its front door answered with, the code it named the class by, the id of the
//! refused request and `Proxy: null`.
//!
//! Why it matters: a `Catch` is written against the documented error name, so a relay that
//! substitutes the wire code for the exception class turns a handled denial into a failed
//! execution — the catcher never fires and the handler state never runs.
//!
//! The rows differ along what the rule generalises over: which service refused the call
//! (Amazon EventBridge, AWS Lambda, Amazon SQS, Amazon SNS and Step Functions itself),
//! whether the invoked operation models the refusal (so whether the relayed class is the
//! modelled error's or the service's base one), which status and code that service answers
//! an unauthorized call with, and which spelling the state's `Catch` lists — the relayed
//! name, the wire code in place of the class, and the base class where the operation models
//! the refusal.
//!
//! The harness (`Chain`), the two renderings of an SDK exception (`refusal_metadata`), the
//! request-id mask (`masked`) and the session principal are the sibling suites':
//! `tests/all/main.rs` makes one crate root of `tests/all/`, so this suite borrows them
//! rather than carrying a second copy. The dimension it adds to them is the ROUTING: that
//! the relayed name is what `ErrorEquals` matches, so the denial is handled and the
//! execution SUCCEEDS down the handler's branch. What a refused publication's error name
//! and cause are, with the far end mounted, is
//! `core/router/tests/all/sfn_events_put_events_authorization.rs`, and the statuses and
//! codes each service answers an unauthorized call with are
//! `tests/all/task_refusal_access_denied_status.rs`; those facts are restated as each row's
//! expectation here deliberately, because a `Catch` can only be held to the name they fix.

use serde_json::{json, Value};

use crate::state_machine_lifecycle::{string, ACCOUNT, REGION};
use crate::task_execution_role::{masked, refusal_metadata, Chain, ACCESS_DENIED_CODE, REQUEST_ID};
use crate::task_refusal_session_principal::{session_principal, unrelated_policy};

/// The status a service that answers an authorization failure as a Forbidden carries, and
/// the one a service whose front door answers every client error as a Bad Request carries
/// for the same refusal.
const FORBIDDEN: u16 = 403;
/// See [`FORBIDDEN`].
const CLIENT_ERROR: u16 = 400;

/// What the handler state puts out, so a denial a `Catch` took over is told apart from one
/// the execution ended on.
const HANDLED: &str = "denied";

/// A one-`Task` definition calling `resource` with `parameters`, whose `Catch` lists the
/// single error name `named` and routes it to a handler keeping what failed.
fn definition(resource: &str, parameters: &Value, named: &str) -> String {
    json!({
        "StartAt": "Call",
        "States": {
            "Call": {
                "Type": "Task",
                "Resource": resource,
                "Parameters": parameters,
                "Catch": [{
                    "ErrorEquals": [named],
                    "ResultPath": "$.err",
                    "Next": "Denied",
                }],
                "End": true,
            },
            "Denied": {
                "Type": "Pass",
                "Parameters": { "handled": HANDLED, "err.$": "$.err" },
                "End": true,
            },
        },
    })
    .to_string()
}

/// The `Error` and `Cause` a terminal execution reports, read where the failure reached:
/// the `Catch` branch's own error output when a catcher took it over — which is also where
/// the handler state's having run is asserted — and the execution's own members when none
/// did.
fn reported(case: &str, described: &Value, caught: bool) -> (String, String) {
    if !caught {
        return (string(described, "error"), string(described, "cause"));
    }
    let output: Value = serde_json::from_str(&string(described, "output"))
        .unwrap_or_else(|why| panic!("case {case:?}: expected a JSON output, got {why}"));
    assert_eq!(
        output.get("handled").and_then(Value::as_str),
        Some(HANDLED),
        "case {case:?}: expected the handler state to have run, got {output}"
    );
    let caught = output
        .get("err")
        .unwrap_or_else(|| panic!("case {case:?}: expected the Catch output in {output}"));
    (string(caught, "Error"), string(caught, "Cause"))
}

/// The id the metadata of `cause` names the refused request by.
fn request_id(case: &str, cause: &str) -> String {
    let named = cause
        .split_once("Request ID: ")
        .map(|(_, named)| named)
        .unwrap_or_else(|| panic!("case {case:?}: expected the refused request named in {cause:?}"));
    named
        .chars()
        .take_while(|character| !";)".contains(*character))
        .collect()
}

/// One row: the refused call, the name its `Catch` lists, and what the execution must
/// report.
struct Row {
    case: &'static str,
    /// The optimized `Resource` the `Task` names, and the parameters it carries.
    resource: &'static str,
    parameters: Value,
    /// The IAM action the refusal names, spelled as the invoked service's own refusal
    /// spells it, and the ARN of what the request addressed.
    action: &'static str,
    target: String,
    /// The error name the state's `Catch` lists, and whether it takes the denial over.
    named: &'static str,
    catches: bool,
    /// The error name the task really failed under, which a `Catch` matches only by naming
    /// it exactly.
    relayed: &'static str,
    /// The name the client an optimized integration is served through gives the invoked
    /// service in the relayed metadata.
    client: &'static str,
    /// The code the invoked service's answer names the class by, and the status it carries.
    code: &'static str,
    status: u16,
}

/// RULE: a `Catch` naming the relayed exception class routes the denial, and one naming the
/// wire code instead matches nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_denied_optimized_task_is_caught_under_the_relayed_exception_class() {
    let chain = Chain::new();
    // No target has to exist: the call is refused before it is made, so these only name
    // what each request addresses.
    let bus_arn = format!("arn:aws:events:{REGION}:{ACCOUNT}:event-bus/default");
    let function_arn = format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:handler");
    let queue_url = format!("https://sqs.{REGION}.amazonaws.com/{ACCOUNT}/intake");
    let queue_arn = format!("arn:aws:sqs:{REGION}:{ACCOUNT}:intake");
    let topic_arn = format!("arn:aws:sns:{REGION}:{ACCOUNT}:orders");
    let child_arn = format!("arn:aws:states:{REGION}:{ACCOUNT}:stateMachine:child");
    // A role whose documents say nothing about any integration call, so every row's call
    // is one the documents in force withhold.
    let role_arn = chain.role("CatchDeniedTaskRole", &unrelated_policy()).await;
    // Every entry of the publication rows: an entry naming no bus publishes onto the
    // default bus, which is the resource the refusal is about.
    let entries = json!([{
        "Source": "sim.catch.denied",
        "DetailType": "denial.caught",
        "Detail": { "marker": "ok" },
    }]);

    let rows = [
        Row {
            // `PutEvents` models no access-denied error of its own, so the class the
            // relayed refusal carries is Amazon EventBridge's base exception.
            case: "a publication caught under the relayed exception class",
            resource: "arn:aws:states:::events:putEvents",
            parameters: json!({ "Entries": entries.clone() }),
            action: "events:PutEvents",
            target: bus_arn.clone(),
            named: "EventBridge.AmazonEventBridgeException",
            catches: true,
            relayed: "EventBridge.AmazonEventBridgeException",
            client: "AmazonEventBridge",
            code: ACCESS_DENIED_CODE,
            status: CLIENT_ERROR,
        },
        Row {
            // The same denial, with the wire code put where the exception class belongs:
            // that is no name this integration writes, so nothing matches it and the
            // execution ends on the refusal.
            case: "the same publication caught under the wire error code",
            resource: "arn:aws:states:::events:putEvents",
            parameters: json!({ "Entries": entries }),
            action: "events:PutEvents",
            target: bus_arn,
            named: "EventBridge.AccessDeniedException",
            catches: false,
            relayed: "EventBridge.AmazonEventBridgeException",
            client: "AmazonEventBridge",
            code: ACCESS_DENIED_CODE,
            status: CLIENT_ERROR,
        },
        Row {
            // A second service whose operation models no access-denied error, answering
            // the refusal as a Forbidden.
            case: "an invocation caught under the relayed exception class",
            resource: "arn:aws:states:::lambda:invoke",
            parameters: json!({ "FunctionName": function_arn.clone(), "Payload": {} }),
            action: "lambda:InvokeFunction",
            target: function_arn.clone(),
            named: "Lambda.AWSLambdaException",
            catches: true,
            relayed: "Lambda.AWSLambdaException",
            client: "AWSLambda",
            code: ACCESS_DENIED_CODE,
            status: FORBIDDEN,
        },
        Row {
            // The wire code in place of the class matches nothing for that service either,
            // so the substitution is no service's property.
            case: "the same invocation caught under the wire error code",
            resource: "arn:aws:states:::lambda:invoke",
            parameters: json!({ "FunctionName": function_arn.clone(), "Payload": {} }),
            action: "lambda:InvokeFunction",
            target: function_arn,
            named: "Lambda.AccessDeniedException",
            catches: false,
            relayed: "Lambda.AWSLambdaException",
            client: "AWSLambda",
            code: ACCESS_DENIED_CODE,
            status: FORBIDDEN,
        },
        Row {
            // Amazon SQS's own front door names the class `AccessDenied` and spells the
            // action it refused in lower case, so the code in the metadata is its own.
            case: "a send caught under the relayed exception class",
            resource: "arn:aws:states:::sqs:sendMessage",
            parameters: json!({ "QueueUrl": queue_url, "MessageBody": "hello" }),
            action: "sqs:sendmessage",
            target: queue_arn,
            named: "SQS.AmazonSQSException",
            catches: true,
            relayed: "SQS.AmazonSQSException",
            client: "AmazonSQS",
            code: "AccessDenied",
            status: FORBIDDEN,
        },
        Row {
            // `Publish` MODELS the refusal of an unauthorized call, so the class is that
            // error's own rather than the service's base one.
            case: "a publish caught under the error its operation models",
            resource: "arn:aws:states:::sns:publish",
            parameters: json!({ "TopicArn": topic_arn.clone(), "Message": "hello" }),
            action: "SNS:Publish",
            target: topic_arn.clone(),
            named: "SNS.AuthorizationErrorException",
            catches: true,
            relayed: "SNS.AuthorizationErrorException",
            client: "AmazonSNS",
            code: "AuthorizationError",
            status: FORBIDDEN,
        },
        Row {
            // The contrast to the first row: where the operation models the refusal, the
            // service's BASE class is not what the failure carries, so a `Catch` naming it
            // matches nothing.
            case: "the same publish caught under the service's base exception class",
            resource: "arn:aws:states:::sns:publish",
            parameters: json!({ "TopicArn": topic_arn.clone(), "Message": "hello" }),
            action: "SNS:Publish",
            target: topic_arn,
            named: "SNS.AmazonSNSException",
            catches: false,
            relayed: "SNS.AuthorizationErrorException",
            client: "AmazonSNS",
            code: "AuthorizationError",
            status: FORBIDDEN,
        },
        Row {
            // And a call of this service itself, whose front door answers an authorization
            // failure as a client error.
            case: "a child execution caught under the relayed exception class",
            resource: "arn:aws:states:::states:startExecution",
            parameters: json!({ "StateMachineArn": child_arn.clone() }),
            action: "states:StartExecution",
            target: child_arn,
            named: "StepFunctions.AWSStepFunctionsException",
            catches: true,
            relayed: "StepFunctions.AWSStepFunctionsException",
            client: "AWSStepFunctions",
            code: ACCESS_DENIED_CODE,
            status: CLIENT_ERROR,
        },
    ];

    let mut named_requests: Vec<String> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let machine = chain
            .machine(
                &format!("catch-denied-{index}"),
                &definition(row.resource, &row.parameters, row.named),
                &role_arn,
            )
            .await;
        let (_execution_arn, described) = chain.ran(&machine, "only", case).await;
        let settled = described.get("status").and_then(Value::as_str);
        match row.catches {
            true => assert_eq!(
                settled,
                Some("SUCCEEDED"),
                "case {case:?}: the Catch on {:?} must handle the denial and end SUCCEEDED, got \
                 {described}",
                row.named
            ),
            false => assert_eq!(
                settled,
                Some("FAILED"),
                "case {case:?}: a Catch on {:?} must not take over a denial reported as {:?}, got \
                 {described}",
                row.named,
                row.relayed
            ),
        }
        let (error, cause) = reported(case, &described, row.catches);
        assert_eq!(
            error, row.relayed,
            "case {case:?}: expected the error name {:?}, got {error:?}",
            row.relayed
        );
        // The whole relayed message: the invoked service's own sentence — whose principal
        // is the session of the execution role the call would have been made as — and then
        // the metadata the client that integration is served through closes it with, up to
        // the id of the refused request, which is one call's own value.
        let principal = session_principal(&role_arn, &cause);
        let expected = format!(
            "User: {principal} is not authorized to perform: {} on resource: {} because no \
             identity-based policy allows the {} action{}",
            row.action,
            row.target,
            row.action,
            refusal_metadata(row.resource, row.client, row.code, row.status)
        );
        let reported_cause = masked(&cause);
        assert_eq!(
            reported_cause, expected,
            "case {case:?}: expected the cause {expected:?}, got {reported_cause:?} (read from \
             {cause:?})"
        );
        // The metadata really names a request: a field carrying none closes straight after
        // it, and nothing in the message spells the mask itself.
        for unnamed in ["Request ID: ;", "Request ID: )", REQUEST_ID] {
            assert!(
                !cause.contains(unnamed),
                "case {case:?}: expected the metadata to name the refused request, got {cause:?}"
            );
        }
        named_requests.push(request_id(case, &cause));
    }

    // The id is a correlation value of each refused call's own, so no two of these refusals
    // share one.
    let mut distinct = named_requests.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        named_requests.len(),
        "expected every refusal to name a request of its own, got {named_requests:?}"
    );
}
