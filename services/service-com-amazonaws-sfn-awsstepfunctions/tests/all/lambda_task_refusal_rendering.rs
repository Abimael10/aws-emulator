// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the METADATA a refused `Task` state's cause closes with, driven
//! over both services' own wire protocols through the production `MultiServiceRouter` with
//! Step Functions and IAM mounted on ONE engine — the shape `core/router/src/lib.rs`
//! mounts them in — so the policies the call is judged against are the documents IAM's own
//! handlers wrote.
//!
//! RULE: a failed `Task`'s cause carries the invoked service's exception as the client that
//! made the call RENDERS it, and which client that is belongs to the integration rather
//! than to the family. The Lambda integration is served by the current AWS SDK, whose
//! message closes with ONE group of comma-separated fields — the service as that SDK names
//! it, the status and the refused request — followed by a group naming how many attempts
//! the call took, and names neither an error code nor a proxy. An integration served by the
//! earlier generation of client closes with semicolon-separated fields instead: that
//! client's own name for the service, the status, the code the answer named the class by,
//! the refused request and `Proxy: null`.
//!
//! The rows differ along what the rule generalises over: the FORM the `Resource` names the
//! call by (a Lambda function's ARN invoked directly, the optimized `lambda:invoke`
//! integration, and the same call through the AWS SDK family), and — as the control that
//! keeps the rule from being "every cause is rendered that way" — one integration of
//! another service, which still reports the earlier client's form.
//!
//! Only the DENIED side is here: the call is refused before it is made, so no target
//! service has to be mounted for the rendering to be observed.

use serde_json::{json, Value};

use crate::state_machine_lifecycle::REGION;
use crate::task_execution_role::{masked, Chain, REQUEST_ID};

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// The status a service whose front door answers an unauthorized call as a Forbidden
/// carries, which both services below do.
const FORBIDDEN: u16 = 403;

/// The code Amazon SQS's own front door names an authorization refusal by, which is not the
/// AWS-common one every other service here answers.
const SQS_REFUSAL_CODE: &str = "AccessDenied";

/// One row: the case, the `Resource` the `Task` names, the parameters it carries, the error
/// name the failed task reports, and the metadata its cause has to close with.
struct Row {
    case: &'static str,
    resource: String,
    parameters: Value,
    error: &'static str,
    metadata: String,
}

/// The metadata the CURRENT AWS SDK closes a refusal's message with, with the refused
/// request's id masked ([`masked`]).
///
/// `service` is the name that SDK's own client gives the invoked service.
fn current_sdk_metadata(service: &str, status: u16) -> String {
    format!(
        " (Service: {service}, Status Code: {status}, Request ID: {REQUEST_ID}) \
         (SDK Attempt Count: 1)"
    )
}

/// The metadata the EARLIER generation of client closes the same refusal with.
fn earlier_client_metadata(client: &str, code: &str, status: u16) -> String {
    format!(
        " (Service: {client}; Status Code: {status}; Error Code: {code}; \
         Request ID: {REQUEST_ID}; Proxy: null)"
    )
}

/// A definition whose one `Task` state makes `resource`'s call with `parameters`.
fn definition(resource: &str, parameters: &Value) -> String {
    json!({
        "StartAt": "Call",
        "States": {
            "Call": {
                "Type": "Task",
                "Resource": resource,
                "Parameters": parameters,
                "End": true,
            }
        }
    })
    .to_string()
}

/// A refused Lambda `Task` reports its cause as the current AWS SDK renders an exception,
/// whichever form its `Resource` named the invocation by; an integration served by the
/// earlier client still reports that client's form.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refusal_is_rendered_by_the_client_its_integration_is_served_through() {
    let chain = Chain::new();
    // Neither target has to exist: authorization is settled before the call is made, so
    // these name what a request addresses and nothing is created for them.
    let function_arn = format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:handler");
    let queue_url = format!("https://sqs.{REGION}.amazonaws.com/{ACCOUNT}/intake");
    // The execution role may write its own logs and nothing else, so every row's call is
    // refused for want of an identity-based grant.
    let role_arn = chain
        .role(
            "RenderingRole",
            &json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Action": ["logs:CreateLogDelivery", "logs:PutLogEvents"],
                    "Resource": "*",
                }],
            }),
        )
        .await;

    let rows = [
        Row {
            case: "a function's ARN invoked directly",
            resource: function_arn.clone(),
            parameters: json!({ "event": "ignored" }),
            error: "Lambda.AWSLambdaException",
            metadata: current_sdk_metadata("Lambda", FORBIDDEN),
        },
        Row {
            case: "the optimized lambda:invoke integration",
            resource: "arn:aws:states:::lambda:invoke".to_string(),
            parameters: json!({ "FunctionName": function_arn.clone(), "Payload": {} }),
            error: "Lambda.AWSLambdaException",
            metadata: current_sdk_metadata("Lambda", FORBIDDEN),
        },
        Row {
            case: "the same invocation through the AWS SDK integration family",
            resource: "arn:aws:states:::aws-sdk:lambda:invoke".to_string(),
            parameters: json!({ "FunctionName": function_arn.clone(), "Payload": {} }),
            error: "Lambda.AWSLambdaException",
            metadata: current_sdk_metadata("Lambda", FORBIDDEN),
        },
        Row {
            // The control: an optimized integration of a service whose client is of the
            // earlier generation reports that client's own rendering.
            case: "an optimized send to a queue, served by the earlier client",
            resource: "arn:aws:states:::sqs:sendMessage".to_string(),
            parameters: json!({ "QueueUrl": queue_url, "MessageBody": "hello" }),
            error: "SQS.AmazonSQSException",
            metadata: earlier_client_metadata("AmazonSQS", SQS_REFUSAL_CODE, FORBIDDEN),
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let machine = chain
            .machine(
                &format!("rendered-{index}"),
                &definition(&row.resource, &row.parameters),
                &role_arn,
            )
            .await;
        let (_execution, described) = chain.ran(&machine, "only", case).await;
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some("FAILED"),
            "case {case:?}: expected the refused call to fail the execution, got {described:?}"
        );
        assert_eq!(
            described.get("error").and_then(Value::as_str),
            Some(row.error),
            "case {case:?}: expected the error {:?}, got {described:?}",
            row.error
        );
        let cause = masked(
            described
                .get("cause")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        );
        assert!(
            cause.ends_with(&row.metadata),
            "case {case:?}: expected the cause to close {:?}, got {cause:?}",
            row.metadata
        );
        // The sentence before the metadata is the service's own refusal, so the whole
        // cause is that sentence and nothing but this one group of fields after it.
        let sentence = cause
            .strip_suffix(&row.metadata)
            .expect("the metadata was just asserted to close the cause");
        assert!(
            !sentence.contains("(Service:"),
            "case {case:?}: expected the metadata to be written once, got {cause:?}"
        );
    }
}

/// The group the current SDK's rendering names the attempt count in is not optional: a
/// client that made the call once still reports it, and the earlier client's fields —
/// an error code and a proxy — appear in neither Lambda cause.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lambda_refusal_names_the_attempts_and_no_error_code_or_proxy() {
    let chain = Chain::new();
    let function_arn = format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:handler");
    let role_arn = chain
        .role(
            "AttemptCountRole",
            &json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Action": "lambda:GetFunction",
                    "Resource": "*",
                }],
            }),
        )
        .await;

    for (index, (case, resource, parameters)) in [
        (
            "a function's ARN invoked directly",
            function_arn.clone(),
            json!({}),
        ),
        (
            "the optimized lambda:invoke integration",
            "arn:aws:states:::lambda:invoke".to_string(),
            json!({ "FunctionName": function_arn.clone(), "Payload": {} }),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let machine = chain
            .machine(
                &format!("attempts-{index}"),
                &definition(&resource, &parameters),
                &role_arn,
            )
            .await;
        let (_execution, described) = chain.ran(&machine, "only", case).await;
        let cause = described
            .get("cause")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            cause.contains("(SDK Attempt Count: 1)"),
            "case {case:?}: expected the cause to name how many attempts the call took, got \
             {cause:?}"
        );
        for absent in ["Error Code:", "Proxy: null", "AWSLambda"] {
            assert!(
                !cause.contains(absent),
                "case {case:?}: expected the cause not to carry {absent:?}, got {cause:?}"
            );
        }
        assert!(
            cause.contains(&format!(
                " (Service: Lambda, Status Code: {FORBIDDEN}, Request ID: "
            )),
            "case {case:?}: expected the current SDK's own metadata, got {cause:?}"
        );
        assert_eq!(
            described.get("error").and_then(Value::as_str),
            Some("Lambda.AWSLambdaException"),
            "case {case:?}: expected Lambda's base exception, got {described:?}"
        );
    }
}
