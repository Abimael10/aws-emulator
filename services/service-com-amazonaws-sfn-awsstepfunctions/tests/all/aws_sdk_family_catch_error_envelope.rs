// SPDX-License-Identifier: CC0-1.0

//! What a `Catch` written against an AWS SDK integration's documented error NAME
//! matches, and which SDK ENVELOPE the failure it catches carries as its `Cause`.
//!
//! The rule under test, for the AWS SDK integration family
//! (`arn:aws:states:::aws-sdk:<sdk name>:<action>`):
//!
//! 1. a service-side refusal is reported as `<service>.<exception class>`, where the
//!    service half is the name the AWS SDK's own client gives the service — its
//!    PascalCase SDK name (`Sns`, `Sqs`), not the service token upper-cased — and the
//!    exception half is the class that answer's error shape resolves to; `ErrorEquals`
//!    matches that name EXACTLY, so a state whose `Catch` lists it routes the failure
//!    and the execution ends `SUCCEEDED`, while one naming the service any other way
//!    does not match and the execution ends `FAILED`;
//! 2. the `Cause` is that exception as the CURRENT AWS SDK renders it: the service's own
//!    sentence, then ONE parenthesised group of COMMA-separated fields — the service as
//!    that SDK names it, the status the answer carried and the refused request — and
//!    then a second group naming how many attempts the call took. It names no error code
//!    and no proxy.
//!
//! The optimized `Resource` of the SAME call is the contrast: it is served by the earlier
//! generation of client, so it reports the other name and the other envelope
//! (semicolon-separated fields, that client's own name for the service, an error code and
//! `Proxy: null`) — which is why a `Catch` written for one family does not match the
//! other.
//!
//! The rows differ along what the rules generalise over: which service answered (Amazon
//! SNS over a form protocol, Amazon SQS over an AWS JSON one), which error class it
//! answered with and therefore which status the envelope carries, which name the `Catch`
//! lists, and which family named the call. The request id each envelope names is that
//! call's own value, so what is held is the text around it and that it carries one.
//!
//! The envelope each row's cause is held to is the one this crate's suites state once,
//! `crate::task_execution_role::refusal_metadata`, read with the refused request's id
//! masked (`crate::task_execution_role::masked`) — so the two renderings are written in
//! one place and cannot drift from what the sibling suites hold.
//!
//! The error names and statuses asserted below are each service's own modelled facts:
//! `smithy_query(query="shape",shape="NotFoundException")` on the Amazon SNS model
//! (`httpError` 404, `awsQueryError` code `NotFound`) and
//! `smithy_query(query="shape",shape="QueueDoesNotExist")` on the Amazon SQS one
//! (`httpError` 400), read through the `Exception` suffix an SDK gives a class whose
//! shape does not already carry it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_sfn_awsstepfunctions::EmulatedService;

use crate::execution_data_encryption::{form_encode, xml_element};
use crate::state_machine_lifecycle::{string, REGION, ROLE_ARN};
use crate::task_execution_role::{masked, refusal_metadata, REQUEST_ID};

/// The two `Resource` spellings of the publish, which name the same call on the same
/// topic through two different clients of Amazon SNS.
const SDK_PUBLISH: &str = "arn:aws:states:::aws-sdk:sns:publish";
/// See [`SDK_PUBLISH`].
const OPTIMIZED_PUBLISH: &str = "arn:aws:states:::sns:publish";
/// The AWS SDK family's spelling of the send, for the second service under test.
const SDK_SEND: &str = "arn:aws:states:::aws-sdk:sqs:sendMessage";

/// The access-key id every request below is signed with: the account's own published
/// credential.
const ACCOUNT_KEY: &str = "AKIDEXAMPLE";

/// Amazon SNS's signing name and `awsQuery` API version, and Amazon SQS's `awsJson1_0`
/// wire facts (`smithy_query(query="service_info")` on each service's own model).
const SNS_SIGNING_NAME: &str = "sns";
const SNS_API_VERSION: &str = "2010-03-31";
const SQS_SIGNING_NAME: &str = "sqs";
const SQS_TARGET_PREFIX: &str = "AmazonSQS";

/// Step Functions' own wire facts.
const SFN_SIGNING_NAME: &str = "states";
const SFN_TARGET_PREFIX: &str = "AWSStepFunctions";

/// Long enough for an execution whose one task was refused to reach a terminal status,
/// whatever else the host is running. Nothing below measures how long anything took: the
/// deadline only bounds the wait.
const PATIENCE: Duration = Duration::from_secs(60);

/// What the handler state puts out, so a handled failure is told apart from one the
/// execution ended on.
const HANDLED: &str = "handled-by-the-catch";

/// Step Functions, Amazon SNS and Amazon SQS over ONE engine — the shape
/// `core/router/src/lib.rs` mounts them in — so the call a task makes is served by the
/// very store this suite's topics and queues were created in.
struct Chain {
    router: MultiServiceRouter,
}

impl Chain {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_sns_amazonsimplenotificationservice::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
            engine,
        ));
        Self { router }
    }

    /// One `awsJson` RPC call against the service `signing_name` names, requiring a 200.
    async fn json(
        &self,
        signing_name: &str,
        target_prefix: &str,
        operation: &str,
        body: &Value,
    ) -> Value {
        let host = format!("{signing_name}.{REGION}.amazonaws.com");
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY}/20260101/{REGION}/{signing_name}/\
             aws4_request, SignedHeaders=host;x-amz-target, Signature=0123456789abcdef"
        );
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://{host}/"))
            .header("host", &host)
            .header("content-type", "application/x-amz-json-1.0")
            .header("x-amz-target", format!("{target_prefix}.{operation}"))
            .header("authorization", authorization)
            .body(Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let parsed: Value = match bytes.is_empty() {
            true => Value::Null,
            false => serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        };
        assert_eq!(
            status, 200,
            "case {operation:?}: expected 200 from {signing_name}, got {status} with {parsed}"
        );
        parsed
    }

    /// Call one Step Functions operation, requiring a 200.
    async fn sfn(&self, operation: &str, body: &Value) -> Value {
        self.json(SFN_SIGNING_NAME, SFN_TARGET_PREFIX, operation, body)
            .await
    }

    /// Call one Amazon SQS operation, requiring a 200.
    async fn sqs(&self, operation: &str, body: &Value) -> Value {
        self.json(SQS_SIGNING_NAME, SQS_TARGET_PREFIX, operation, body)
            .await
    }

    /// One Amazon SNS `awsQuery` form call, the way a client of such a service makes it,
    /// requiring a 200.
    async fn sns(&self, action: &str, params: &[(&str, String)]) -> String {
        let mut body = format!("Action={action}&Version={SNS_API_VERSION}");
        for (name, value) in params {
            body.push('&');
            body.push_str(name);
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let host = format!("{SNS_SIGNING_NAME}.{REGION}.amazonaws.com");
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY}/20260101/{REGION}/{SNS_SIGNING_NAME}/\
             aws4_request, SignedHeaders=host, Signature=0123456789abcdef"
        );
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://{host}/"))
            .header("host", host)
            .header("content-type", "application/x-www-form-urlencoded")
            .header("authorization", authorization)
            .body(Bytes::from(body))
            .expect("a well-formed awsQuery request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let body = String::from_utf8_lossy(&response.into_body()).to_string();
        assert_eq!(
            status, 200,
            "case {action:?}: expected 200 from Amazon SNS, got {status} with {body}"
        );
        body
    }

    /// A topic called `name`, by its ARN.
    async fn topic(&self, name: &str) -> String {
        let created = self.sns("CreateTopic", &[("Name", name.to_string())]).await;
        xml_element(&created, "TopicArn")
            .unwrap_or_else(|| panic!("CreateTopic answers a TopicArn, got {created}"))
    }

    /// A queue called `name`, by its url.
    async fn queue(&self, name: &str) -> String {
        let created = self.sqs("CreateQueue", &json!({ "QueueName": name })).await;
        string(&created, "QueueUrl")
    }

    /// Run `definition` once as the state machine `machine`, and describe the execution
    /// it settled as.
    async fn ran(&self, case: &str, machine: &str, definition: &Value) -> Value {
        let created = self
            .sfn(
                "CreateStateMachine",
                &json!({
                    "name": machine,
                    "definition": definition.to_string(),
                    "roleArn": ROLE_ARN,
                    "type": "STANDARD",
                }),
            )
            .await;
        let machine_arn = string(&created, "stateMachineArn");
        let started = self
            .sfn(
                "StartExecution",
                &json!({ "stateMachineArn": machine_arn, "name": "once", "input": "{}" }),
            )
            .await;
        let execution_arn = string(&started, "executionArn");
        let began = Instant::now();
        loop {
            let described = self
                .sfn("DescribeExecution", &json!({ "executionArn": execution_arn }))
                .await;
            if described.get("status").and_then(Value::as_str) != Some("RUNNING") {
                return described;
            }
            assert!(
                began.elapsed() < PATIENCE,
                "case {case:?}: expected a terminal status within {PATIENCE:?}, last saw \
                 {described}"
            );
            ::tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

/// A one-`Task` definition calling `resource` with `parameters`, whose `Catch` lists the
/// single error name `named` and routes it to a handler that keeps what failed.
fn definition(resource: &str, parameters: &Value, named: &str) -> Value {
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
                    "Next": "Handled",
                }],
                "End": true,
            },
            "Handled": {
                "Type": "Pass",
                "Parameters": { "handled": HANDLED, "err.$": "$.err" },
                "End": true,
            },
        },
    })
}

/// The `Error` and `Cause` a terminal execution reports, taken from where the failure
/// reached: the execution's own members when nothing handled it, and the `Catch` path's
/// error output when one did.
fn reported(case: &str, described: &Value, caught: bool) -> (String, String) {
    if !caught {
        return (string(described, "error"), string(described, "cause"));
    }
    let output = described
        .get("output")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected an output in {described}"));
    let output: Value = ::serde_json::from_str(output)
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

/// Hold `cause` to `sentence` followed by the metadata the client the `Resource`
/// `resource` belongs to is served by closes an exception's message with
/// ([`refusal_metadata`]), with the refused request's id masked ([`masked`]): that id is
/// one call's own value, so what is held is the whole text around it — and the field has
/// to carry one, which is asserted of the cause as it came.
fn assert_cause(
    case: &str,
    cause: &str,
    resource: &str,
    client: &str,
    sentence: &str,
    status: u16,
    code: Option<&str>,
) {
    let sdk_family = resource.contains(":aws-sdk:");
    assert_eq!(
        code.is_none(),
        sdk_family,
        "case {case:?}: the wire code is a field of the earlier client's envelope alone"
    );
    let expected = format!(
        "{sentence}{}",
        refusal_metadata(resource, client, code.unwrap_or_default(), status)
    );
    let reported = masked(cause);
    assert_eq!(
        reported, expected,
        "case {case:?}: expected the cause {expected:?}, got {reported:?} (read from {cause:?})"
    );
    // The id the mask stood in for is a value of this call's own: a field carrying none
    // closes straight after it, and nothing in the message spells the mask itself.
    for unnamed in ["Request ID: ;", "Request ID: )", REQUEST_ID] {
        assert!(
            !cause.contains(unnamed),
            "case {case:?}: expected the metadata to name the refused request, got {cause:?}"
        );
    }
    // The current SDK's rendering states no error code and no proxy, and separates its
    // own fields with commas rather than semicolons: the fields the earlier client wrote
    // are the ones a caller parsing this cause would read wrongly.
    if sdk_family {
        for absent in ["Error Code", "Proxy", ";"] {
            assert!(
                !cause.contains(absent),
                "case {case:?}: the current SDK's envelope states no {absent:?}, got {cause:?}"
            );
        }
    }
}

/// One row: the refused call, the name its `Catch` lists, and what the execution must
/// report.
struct Row {
    case: &'static str,
    /// The state machine the work is for; its name has to be unique in the account.
    machine: &'static str,
    resource: &'static str,
    parameters: Value,
    /// The error name the state's `Catch` lists.
    named: &'static str,
    /// The error name the task really failed under, which a `Catch` matches only by
    /// naming it exactly.
    reported: &'static str,
    /// Whether that `Catch` takes the failure over.
    catches: bool,
    /// The name the client the `Resource`'s own family is served by gives the invoked
    /// service in the envelope.
    client: &'static str,
    /// The invoked service's own sentence for the refusal, and the status its answer
    /// carried.
    sentence: &'static str,
    status: u16,
    /// The wire code the envelope names the class by, which is a field of the earlier
    /// client's rendering alone — `None` for a row whose envelope states none.
    code: Option<&'static str>,
}

/// RULES 1 and 2: a `Catch` naming the AWS SDK family's own error name routes the
/// refusal, and the failure it hands the state carries the current SDK's envelope.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_aws_sdk_task_failure_is_named_and_rendered_by_the_sdk_that_served_it() {
    let chain = Chain::new();
    // A topic that exists, and a well-formed ARN of this account naming no topic — so
    // the refusal is about the name the publish addresses and not about the account
    // holding no topics at all.
    let topic = chain.topic("catch-envelope-orders").await;
    let absent_topic = format!("{topic}-absent");
    let queue = chain.queue("catch-envelope-queue").await;
    let absent_queue = queue.replace("catch-envelope-queue", "catch-envelope-nowhere");
    assert_ne!(
        queue, absent_queue,
        "the url of a queue nobody created has to differ from the one that exists"
    );

    let publish = json!({ "TopicArn": absent_topic, "Message": "never" });
    let send = json!({ "QueueUrl": absent_queue, "MessageBody": "never" });
    // Amazon SNS answers a topic it does not hold with its own fixed sentence; Amazon
    // SQS answers the one it states for a queue url that resolves to nothing.
    const NO_TOPIC: &str = "Topic does not exist";
    const NO_QUEUE: &str = "The specified queue does not exist.";

    let rows = [
        Row {
            case: "the publish, caught under the name the AWS SDK writes",
            machine: "catch-envelope-sdk-own",
            resource: SDK_PUBLISH,
            parameters: publish.clone(),
            named: "Sns.NotFoundException",
            reported: "Sns.NotFoundException",
            catches: true,
            client: "Sns",
            sentence: NO_TOPIC,
            status: 404,
            code: None,
        },
        Row {
            // The same refusal, with the service named by its token upper-cased: that
            // is no name the AWS SDK family writes, so nothing matches it.
            case: "the publish, caught under the service token upper-cased",
            machine: "catch-envelope-sdk-upper",
            resource: SDK_PUBLISH,
            parameters: publish.clone(),
            named: "SNS.NotFoundException",
            reported: "Sns.NotFoundException",
            catches: false,
            client: "Sns",
            sentence: NO_TOPIC,
            status: 404,
            code: None,
        },
        Row {
            // A second service, a second error class and a second status, so neither
            // rule is one service's property: Amazon SQS models this error's shape
            // WITHOUT the suffix its class carries.
            case: "the send to a queue the account does not hold, through the AWS SDK family",
            machine: "catch-envelope-sdk-queue",
            resource: SDK_SEND,
            parameters: send,
            named: "Sqs.QueueDoesNotExistException",
            reported: "Sqs.QueueDoesNotExistException",
            catches: true,
            client: "Sqs",
            sentence: NO_QUEUE,
            status: 400,
            code: None,
        },
        Row {
            // The contrast: the same call through the optimized `Resource`, served by
            // the earlier generation of client — it writes the other name, so the AWS
            // SDK family's name matches nothing here.
            case: "the publish through the optimized Resource, caught the AWS SDK family's way",
            machine: "catch-envelope-optimized-other",
            resource: OPTIMIZED_PUBLISH,
            parameters: publish.clone(),
            named: "Sns.NotFoundException",
            reported: "SNS.NotFoundException",
            catches: false,
            client: "AmazonSNS",
            sentence: NO_TOPIC,
            status: 404,
            // The legacy query code Amazon SNS binds this error shape to
            // (`smithy_query(query="shape",shape="NotFoundException")`: awsQueryError
            // `NotFound`), which only the earlier client's envelope states.
            code: Some("NotFound"),
        },
        Row {
            case: "the publish through the optimized Resource, caught under its own name",
            machine: "catch-envelope-optimized-own",
            resource: OPTIMIZED_PUBLISH,
            parameters: publish,
            named: "SNS.NotFoundException",
            reported: "SNS.NotFoundException",
            catches: true,
            client: "AmazonSNS",
            sentence: NO_TOPIC,
            status: 404,
            code: Some("NotFound"),
        },
    ];

    for row in rows {
        let Row {
            case,
            machine,
            resource,
            parameters,
            named,
            reported: expected,
            catches,
            client,
            sentence,
            status,
            code,
        } = row;
        let described = chain
            .ran(case, machine, &definition(resource, &parameters, named))
            .await;
        let settled = described.get("status").and_then(Value::as_str);
        match catches {
            true => assert_eq!(
                settled,
                Some("SUCCEEDED"),
                "case {case:?}: the Catch on {named:?} must handle the failure and end \
                 SUCCEEDED, got {described}"
            ),
            false => assert_eq!(
                settled,
                Some("FAILED"),
                "case {case:?}: a Catch on {named:?} must not take over a failure reported as \
                 {expected:?}, got {described}"
            ),
        }
        let (error, cause) = reported(case, &described, catches);
        assert_eq!(
            error, expected,
            "case {case:?}: expected the error name {expected:?}, got {error:?}"
        );
        assert_cause(case, &cause, resource, client, sentence, status, code);
    }
}
