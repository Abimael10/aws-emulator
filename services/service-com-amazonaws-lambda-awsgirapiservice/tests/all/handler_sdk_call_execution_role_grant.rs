// SPDX-License-Identifier: CC0-1.0

//! The permission boundary of the calls a RUNNING handler makes with the AWS SDK its runtime
//! supplies, end to end over the wire: Lambda, Amazon SQS, Amazon SNS, IAM and the token
//! service on ONE engine, a `python3.x` function whose code does `import boto3`, and the SAME
//! calls made first by an execution role that is granted none of them and then by one that is
//! granted some.
//!
//! The RULE pinned here: a managed runtime puts the execution role's temporary credentials in
//! the execution environment, so a client built inside the handler is signed as that role's
//! session — `arn:<partition>:sts::<account>:assumed-role/<role name>/<function name>` — and
//! the service that SERVES the call decides it against the identity-based policies in force on
//! that role at the instant of the call. A call those policies allow is served and does its
//! work; a call they do not allow is refused with the serving service's own authorization
//! refusal, naming that principal, the action and the resource the call addressed, and does
//! none of the work.
//!
//! The rows differ along what the rule generalises over: WHICH service serves the call (Amazon
//! SQS, whose model declares an AWS JSON protocol and which has no modelled denial so it
//! refuses with the service-common `AccessDenied` at HTTP 403, and Amazon SNS, whose `awsQuery`
//! model declares `AuthorizationErrorException` with the wire code `AuthorizationError` at the
//! same status), WHICH kind of operation it is (a write, a read that consumes, a batch write
//! decided under its single-message action, a reporting read of the resource's own attributes,
//! a write that creates a resource), and whether the policies in force cover the action the
//! call performs. Both halves of every row are asserted — what the SDK gave the guest, which
//! the handler returns as the invocation's payload, and what the serving service holds
//! afterwards, read where a client reads it — so a refusal that still did the work, or a grant
//! that did not, fails here.
//!
//! The suite RUNS a handler, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]), and it makes TWO
//! invocations in all — the calls under no grant and the calls under the grant — because an
//! invocation of its own costs this target a share of the run budget every other invocation
//! suite draws on.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoding, the XML element reader and the IAM call every suite in this
// target writes a role with, owned by the suite that first needed them rather than copied.
use crate::event_source_mapping_role_access::{element, form_encode};
// The queue reads and writes this target makes with the account's own published credential,
// and the Lambda calls every suite makes.
use crate::event_source_mappings::{
    await_function_active, call, create_queue, queue_message_counts, router_on, send_message,
};
// The one .zip writer this target packages a running handler with.
use crate::synchronous_invoke_run::package;

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The account every ARN below names, which is the account this emulator serves.
const ACCOUNT: &str = "000000000000";

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The execution role the function runs as, the function's own name, and the inline identity
/// policy the role carries the grant in.
const ROLE_NAME: &str = "lambda-sdk-call-boundary";
const FUNCTION: &str = "sdk-call-boundary";
const INLINE_POLICY: &str = "granted-calls";

/// The AWS-managed policy a Lambda execution role is created with: it grants the log writes a
/// function makes and nothing else, so a role carrying only it has no grant on either service
/// called below.
const BASIC_EXECUTION_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole";

/// The queue the handler writes to and reads, and the topic it subscribes that queue to.
const QUEUE: &str = "boundary-queue";
const TOPIC: &str = "boundary-topic";

/// The message the SUITE puts on the queue before the first invocation, which is what the
/// refused read must leave where it is and the granted read must hand over.
const SEEDED_BODY: &str = "seeded-before-any-grant";

/// The bodies the handler sends once the grant is in force: one single send and one batch.
const SENT_BODY: &str = "sent-under-the-grant";
const BATCHED_BODIES: [&str; 2] = ["batched-one", "batched-two"];

/// The error code and status Amazon SQS refuses a call the policies in force do not allow
/// with: its model declares no denial shape, so the refusal is the service-common one.
const SQS_DENIAL_CODE: &str = "AccessDenied";

/// The wire error code Amazon SNS refuses one with: its model's `AuthorizationErrorException`,
/// whose `awsQueryError` code is this (`smithy_query(query="shape",
/// shape="AuthorizationErrorException")`).
const SNS_DENIAL_CODE: &str = "AuthorizationError";

/// The status both of those refusals carry (`@httpError(403)` on the SNS shape, and the status
/// the service-common refusal answers with).
const DENIAL_STATUS: u16 = 403;

/// How long a round trip through another service is waited for: the condition is looked at
/// again until it holds or this budget is spent, so nothing here is timed.
const CLOCK_BUDGET: Duration = Duration::from_secs(30);

/// How long between two looks.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// Lambda, Amazon SQS, Amazon SNS, IAM and the token service over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them: the handler's SDK call is dispatched back into this
/// same router, so the queue and topic it addresses are the ones this suite reads.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_sns_amazonsimplenotificationservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// One awsQuery call to Amazon SNS that must be served, as the XML it answered. The suite's own
/// calls present the account's own published credential, which holds no identity policies.
async fn sns(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version=2010-03-31");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sns.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEMULATOR/20260101/{REGION}/sns/aws4_request, \
                 SignedHeaders=host, Signature=not-verified"
            ),
        )
        .body(bytes::Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "setup: expected HTTP 200 from SNS {action}, got {status} / {text}"
    );
    text
}

/// One IAM call that must be served.
async fn iam(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) {
    let (status, text) = crate::event_source_mapping_role_access::iam(router, action, params).await;
    assert_eq!(
        status, 200,
        "setup: expected HTTP 200 from IAM {action}, got {status} / {text}"
    );
}

/// The session ARN AWS reports for the calls a function's code makes: the role's own name and
/// the function's, under the token service's assumed-role form
/// (`state::execution::execution_role_session`).
fn session_principal() -> String {
    format!("arn:aws:sts::{ACCOUNT}:assumed-role/{ROLE_NAME}/{FUNCTION}")
}

/// The ARN of the execution role, as IAM records it.
fn role_arn() -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/{ROLE_NAME}")
}

/// The trust policy a Lambda execution role carries.
fn trust_document() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "lambda.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// The identity policy that grants the role the SEND and the READ of one queue and the
/// SUBSCRIBE of one topic — and nothing else, so a call outside it is still refused while the
/// role holds this document.
fn grant_document(queue_arn: &str, topic_arn: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": ["sqs:SendMessage", "sqs:ReceiveMessage"],
                "Resource": queue_arn,
            },
            {
                "Effect": "Allow",
                "Action": "sns:Subscribe",
                "Resource": topic_arn,
            },
        ],
    })
    .to_string()
}

/// The refusal a request no readable identity-based document allows carries, as IAM words it:
/// the principal that signed, the action it asked for, the resource it addressed, and the fact
/// that no identity-based policy allows that action.
fn implicit_denial(action: &str, resource: &str) -> String {
    format!(
        "User: {} is not authorized to perform: {action} on resource: {resource} because no \
         identity-based policy allows the {action} action",
        session_principal()
    )
}

/// The handler every invocation runs: it builds a client for each call the event names, makes
/// it, and reports what the SDK gave it — so the invocation's payload IS what the guest saw.
///
/// A service the runtime's SDK resolves no client for, and any other error the SDK raises
/// before a service answered, are reported as themselves rather than swallowed: neither is an
/// answer any AWS service gives, so a row that hits one fails with what it was.
const HANDLER_SOURCE: &str = r#"
import boto3
import botocore.exceptions


def lambda_handler(event, context):
    results = []
    for asked in event["calls"]:
        try:
            client = boto3.client(asked["service"])
        except botocore.exceptions.UnknownServiceError as error:
            results.append({"ok": False, "no_client_for": asked["service"], "detail": str(error)})
            continue
        try:
            answered = getattr(client, asked["method"])(**asked["parameters"])
        except botocore.exceptions.ClientError as error:
            results.append(
                {
                    "ok": False,
                    "code": error.response["Error"]["Code"],
                    "message": error.response["Error"].get("Message"),
                    "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
                }
            )
            continue
        except botocore.exceptions.BotoCoreError as error:
            results.append(
                {"ok": False, "sdk_error": type(error).__name__, "detail": str(error)}
            )
            continue
        results.append(
            {
                "ok": True,
                "status": answered["ResponseMetadata"]["HTTPStatusCode"],
                "answered": {
                    name: value
                    for name, value in answered.items()
                    if name != "ResponseMetadata"
                },
            }
        )
    return {"results": results}
"#;

/// One row: the call the handler makes, and what the policies in force mean for it.
struct Row {
    case: &'static str,
    /// The service token the handler builds its client with.
    service: &'static str,
    method: &'static str,
    parameters: Value,
    /// For a call the policies in force do NOT allow: the error code the serving service
    /// refuses it with, and the action and the resource its refusal names. `None` for a call
    /// they do allow.
    refused: Option<(&'static str, String, String)>,
    /// The output members a served call must carry, each present and not null.
    present: &'static [&'static str],
}

/// The IAM action each call below is authorized under, spelled as the SERVING service names it
/// in a refusal: Amazon SQS case-folds the action (`state::authorization`'s `reported_action`),
/// and Amazon SNS publishes its actions under the `SNS:` prefix.
const SEND: &str = "sqs:sendmessage";
const RECEIVE: &str = "sqs:receivemessage";
const QUEUE_ATTRIBUTES: &str = "sqs:getqueueattributes";
const SUBSCRIBE: &str = "SNS:Subscribe";

/// The calls as the handler asks for them, each named by the method and the parameters it
/// carries, so one package serves every row.
fn send_parameters(queue_url: &str) -> Value {
    json!({ "QueueUrl": queue_url, "MessageBody": SENT_BODY })
}

fn batch_parameters(queue_url: &str) -> Value {
    json!({
        "QueueUrl": queue_url,
        "Entries": [
            { "Id": "one", "MessageBody": BATCHED_BODIES[0] },
            { "Id": "two", "MessageBody": BATCHED_BODIES[1] },
        ],
    })
}

fn receive_parameters(queue_url: &str) -> Value {
    json!({ "QueueUrl": queue_url, "MaxNumberOfMessages": 1 })
}

fn attributes_parameters(queue_url: &str) -> Value {
    json!({ "QueueUrl": queue_url, "AttributeNames": ["QueueArn"] })
}

fn subscribe_parameters(topic_arn: &str, queue_arn: &str) -> Value {
    json!({
        "TopicArn": topic_arn,
        "Protocol": "sqs",
        "Endpoint": queue_arn,
        "ReturnSubscriptionArn": true,
    })
}

/// The rows of the FIRST invocation: the role carries only the managed policy every execution
/// role carries, so every call is outside the policies in force.
fn withheld_rows(queue_url: &str, queue_arn: &str, topic_arn: &str) -> Vec<Row> {
    vec![
        Row {
            case: "a write no identity policy allows",
            service: "sqs",
            method: "send_message",
            parameters: send_parameters(queue_url),
            refused: Some((SQS_DENIAL_CODE, SEND.to_string(), queue_arn.to_string())),
            present: &[],
        },
        Row {
            case: "a batch write, decided under its single-message action",
            service: "sqs",
            method: "send_message_batch",
            parameters: batch_parameters(queue_url),
            refused: Some((SQS_DENIAL_CODE, SEND.to_string(), queue_arn.to_string())),
            present: &[],
        },
        Row {
            case: "a read that would consume, no identity policy allows",
            service: "sqs",
            method: "receive_message",
            parameters: receive_parameters(queue_url),
            refused: Some((SQS_DENIAL_CODE, RECEIVE.to_string(), queue_arn.to_string())),
            present: &[],
        },
        Row {
            case: "a reporting read of the resource's own attributes",
            service: "sqs",
            method: "get_queue_attributes",
            parameters: attributes_parameters(queue_url),
            refused: Some((
                SQS_DENIAL_CODE,
                QUEUE_ATTRIBUTES.to_string(),
                queue_arn.to_string(),
            )),
            present: &[],
        },
        Row {
            case: "a write of another service, which refuses with its own modelled code",
            service: "sns",
            method: "subscribe",
            parameters: subscribe_parameters(topic_arn, queue_arn),
            refused: Some((
                SNS_DENIAL_CODE,
                SUBSCRIBE.to_string(),
                topic_arn.to_string(),
            )),
            present: &[],
        },
    ]
}

/// The rows of the SECOND invocation, with the grant in force: the same calls, of which the
/// ones the document covers are now served and the one it does not is still refused.
///
/// The read comes first, so what it hands over is the one message the queue held before any
/// grant and the sends that follow it are observed on their own.
fn granted_rows(queue_url: &str, queue_arn: &str, topic_arn: &str) -> Vec<Row> {
    vec![
        Row {
            case: "a read the identity policy allows",
            service: "sqs",
            method: "receive_message",
            parameters: receive_parameters(queue_url),
            refused: None,
            present: &["Messages"],
        },
        Row {
            case: "a write the identity policy allows",
            service: "sqs",
            method: "send_message",
            parameters: send_parameters(queue_url),
            refused: None,
            present: &["MessageId"],
        },
        Row {
            case: "a batch write allowed by its single-message action",
            service: "sqs",
            method: "send_message_batch",
            parameters: batch_parameters(queue_url),
            refused: None,
            present: &["Successful"],
        },
        Row {
            case: "a reporting read the identity policy still does not allow",
            service: "sqs",
            method: "get_queue_attributes",
            parameters: attributes_parameters(queue_url),
            refused: Some((
                SQS_DENIAL_CODE,
                QUEUE_ATTRIBUTES.to_string(),
                queue_arn.to_string(),
            )),
            present: &[],
        },
        Row {
            case: "a write of another service the identity policy allows",
            service: "sns",
            method: "subscribe",
            parameters: subscribe_parameters(topic_arn, queue_arn),
            refused: None,
            present: &["SubscriptionArn"],
        },
    ]
}

/// Make every row's call in ONE invocation and assert what the SDK gave the guest for each,
/// answering the results so a row's own output can be read further.
async fn calls_made(router: &MultiServiceRouter, label: &str, rows: &[Row]) -> Vec<Value> {
    let asked: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "service": row.service,
                "method": row.method,
                "parameters": row.parameters,
            })
        })
        .collect();
    let answered = call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{FUNCTION}/invocations"),
        &json!({ "calls": asked }),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "case {label:?}: expected HTTP 200 from Invoke, got {} ({})",
        answered.status,
        answered.text()
    );
    let signal = answered.header("x-amz-function-error");
    assert_eq!(
        signal, None,
        "case {label:?}: expected no FunctionError, got {signal:?} with {}",
        answered.text()
    );
    let payload = answered.json();
    let results = payload["results"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        results.len(),
        rows.len(),
        "case {label:?}: expected one result per call, got {payload}"
    );
    for (row, result) in rows.iter().zip(&results) {
        let case = row.case;
        match &row.refused {
            Some((code, action, resource)) => {
                assert_eq!(
                    result["ok"],
                    json!(false),
                    "case {case:?}: expected the call to be refused, got {result}"
                );
                assert_eq!(
                    result["code"],
                    json!(code),
                    "case {case:?}: expected the error code {code:?}, got {result}"
                );
                assert_eq!(
                    result["status"],
                    json!(DENIAL_STATUS),
                    "case {case:?}: expected HTTP {DENIAL_STATUS} on the refusal, got {result}"
                );
                assert_eq!(
                    result["message"],
                    json!(implicit_denial(action, resource)),
                    "case {case:?}: expected the refusal to name the session, the action and \
                     the resource, got {result}"
                );
            }
            None => {
                assert_eq!(
                    result["ok"],
                    json!(true),
                    "case {case:?}: expected the call to be served, got {result}"
                );
                assert_eq!(
                    result["status"],
                    json!(200),
                    "case {case:?}: expected the SDK to report HTTP 200, got {result}"
                );
                for member in row.present {
                    assert!(
                        !result["answered"][*member].is_null(),
                        "case {case:?}: expected the output member {member:?} to be answered, \
                         got {result}"
                    );
                }
            }
        }
    }
    results
}

/// Every message the queue at `url` holds once `expected` of them are visible, looked at again
/// until they are or the budget is spent — so what is waited for is the CONDITION and not a
/// span.
async fn visible_bodies(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    expected: usize,
) -> Vec<String> {
    let deadline = std::time::Instant::now() + CLOCK_BUDGET;
    let mut held: Vec<String> = Vec::new();
    loop {
        let (_status, received) = crate::event_source_mappings::call_sqs(
            router,
            "ReceiveMessage",
            &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
        )
        .await;
        for message in received["Messages"].as_array().cloned().unwrap_or_default() {
            if let Some(body) = message["Body"].as_str() {
                held.push(body.to_string());
            }
        }
        if held.len() >= expected {
            return held;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected {expected} message(s) on the queue, got {held:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// RULE: a call a running handler makes with the SDK its runtime supplies is signed as the
/// function's execution-role session, so the service that serves it decides it against the
/// identity-based policies in force on that role — serving the calls they allow and refusing
/// the others with its own authorization refusal, which does none of the work.
#[tokio::test]
async fn a_handlers_own_sdk_calls_are_decided_against_its_execution_roles_policies() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let router = router();

    // The role the function runs as, written with IAM's own operations, so the session the
    // runtime mints speaks for the record `iam:CreateRole` wrote. It carries only the managed
    // policy every execution role carries, which grants nothing on either service called.
    iam(
        &router,
        "CreateRole",
        &[
            ("RoleName", ROLE_NAME),
            ("AssumeRolePolicyDocument", &trust_document()),
        ],
    )
    .await;
    iam(
        &router,
        "AttachRolePolicy",
        &[
            ("RoleName", ROLE_NAME),
            ("PolicyArn", BASIC_EXECUTION_POLICY),
        ],
    )
    .await;

    // The queue and the topic the handler addresses, each created where a client creates one.
    let (queue_url, queue_arn) = create_queue(&router, "setup", QUEUE, &json!({})).await;
    let topic_arn = element(&sns(&router, "CreateTopic", &[("Name", TOPIC)]).await, "TopicArn");
    assert_eq!(
        topic_arn,
        format!("arn:aws:sns:{REGION}:{ACCOUNT}:{TOPIC}"),
        "setup: expected the topic ARN of this account and Region, got {topic_arn}"
    );

    // One message on the queue before any grant: what the refused read must leave where it is.
    send_message(&router, "setup", &queue_url, SEEDED_BODY, None).await;

    // The function itself, running as that role.
    let created = call(
        &router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": FUNCTION,
            "Role": role_arn(),
            "Runtime": "python3.12",
            "Handler": "lambda_function.lambda_handler",
            "Timeout": 30,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(HANDLER_SOURCE)),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "setup: expected HTTP 201 from CreateFunction, got {} ({})",
        created.status,
        created.text()
    );
    await_function_active(&router, "setup", FUNCTION).await;

    // The first invocation: every call is outside the policies in force.
    let withheld = withheld_rows(&queue_url, &queue_arn, &topic_arn);
    calls_made(&router, "the calls under no grant", &withheld).await;

    // The other half of each refused row: the serving services did none of the work. The queue
    // still holds exactly the one message the suite put there — nothing was added by the
    // refused sends, and nothing taken in flight by the refused read — and the topic has no
    // subscription.
    let counts = queue_message_counts(&router, "the calls under no grant", &queue_url).await;
    assert_eq!(
        counts,
        (1, 0, 0),
        "case \"the calls under no grant\": expected the queue to hold only the seeded message, \
         visible and not in flight, got {counts:?}"
    );
    let listed = sns(
        &router,
        "ListSubscriptionsByTopic",
        &[("TopicArn", &topic_arn)],
    )
    .await;
    assert!(
        !listed.contains(&queue_arn),
        "case \"a write of another service, which refuses with its own modelled code\": \
         expected the refused subscribe to create no subscription, got {listed}"
    );

    // The grant, written where a client writes one.
    iam(
        &router,
        "PutRolePolicy",
        &[
            ("RoleName", ROLE_NAME),
            ("PolicyName", INLINE_POLICY),
            ("PolicyDocument", &grant_document(&queue_arn, &topic_arn)),
        ],
    )
    .await;

    // The second invocation: the same calls, decided against the document just written.
    let granted = granted_rows(&queue_url, &queue_arn, &topic_arn);
    let results = calls_made(&router, "the calls under the grant", &granted).await;

    // What the granted read handed over is the message the queue held, so the read really read
    // this queue rather than answering an empty page.
    let read = &results[0]["answered"]["Messages"][0]["Body"];
    assert_eq!(
        read,
        &json!(SEEDED_BODY),
        "case \"a read the identity policy allows\": expected the seeded message, got {}",
        results[0]
    );

    // What the granted subscribe created: the ARN it answered is of the topic it named, and the
    // subscription is the one that topic reports.
    let subscription = results[4]["answered"]["SubscriptionArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        subscription.starts_with(&format!("{topic_arn}:")),
        "case \"a write of another service the identity policy allows\": expected a subscription \
         ARN of that topic, got {subscription:?}"
    );
    let listed = sns(
        &router,
        "ListSubscriptionsByTopic",
        &[("TopicArn", &topic_arn)],
    )
    .await;
    assert!(
        listed.contains(&subscription) && listed.contains(&queue_arn),
        "case \"a write of another service the identity policy allows\": expected the service to \
         report the subscription the handler created, got {listed}"
    );

    // And what the granted writes left: the single send and both batch entries are on the
    // queue, read where a client reads them.
    let bodies = visible_bodies(
        &router,
        "a write the identity policy allows",
        &queue_url,
        1 + BATCHED_BODIES.len(),
    )
    .await;
    for expected in [SENT_BODY, BATCHED_BODIES[0], BATCHED_BODIES[1]] {
        assert!(
            bodies.iter().any(|body| body == expected),
            "case \"a write the identity policy allows\": expected {expected:?} on the queue, \
             got {bodies:?}"
        );
    }
}
