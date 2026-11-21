// SPDX-License-Identifier: CC0-1.0

//! What a RUNNING function's own notification client does ACROSS a change to the policies in
//! force on its execution role: Lambda, Amazon SNS, Amazon SQS, IAM and the token service on
//! ONE engine, a `python3.x` function that builds its Amazon SNS client at MODULE scope, and
//! the same publications made before the grant is written and again after it.
//!
//! RULE: a managed runtime carries the AWS SDK on the handler's import path with the catalog of
//! every service the runtime can call, so a client the handler builds for one of them is
//! CONSTRUCTED and its call is signed as a session of the function's execution role and decided
//! by the serving service against the policies in force on that role AT THE INSTANT OF THE CALL
//! — so a call no statement covers is refused with that service's own authorization code
//! (Amazon SNS: `AuthorizationError`, the `awsQueryError` code of `AuthorizationErrorException`,
//! at HTTP 403) and does nothing, and once a statement covering it is written the NEXT call is
//! served, whatever document carries the statement and however it names the resource. Nothing
//! is cached in either direction: not the client, not the session, and not the decision.
//!
//! Why that is one rule and not two: a handler whose client cannot be built at all fails
//! identically with and without the grant, so "refused" and "no such client" become
//! indistinguishable and a least-privilege policy cannot be validated from inside a function.
//! Pinning both phases of the same call through the same module-scope client is what keeps
//! those two apart: phase one must be the SERVICE's refusal, with its own code and status,
//! rather than anything the SDK's own client registry decided.
//!
//! Five crates have to agree for that to hold — IAM holds the role and the documents the grant
//! arrives in, the token service mints the session the handler's calls are signed with, Lambda
//! runs the handler and supplies it the SDK, Amazon SNS takes the decision and fans an admitted
//! publication out, and Amazon SQS holds the queue that receives it — which is why the suite
//! stands up all five on one engine.
//!
//! The rows differ along what the rule generalises over: WHICH document the grant arrives in
//! (an inline role policy, and a customer-managed policy attached to the role), how that
//! document's statement names the topic (named exactly, and covered by a resource pattern), a
//! topic no document ever covers, and the TEXT each publication carries (one plain line, a JSON
//! document, text outside ASCII). Every row asserts both halves of both phases — what the SDK
//! gave the handler, and what the subscribed queue holds afterwards — so a refusal that
//! published anyway, and a grant that answered a `MessageId` and delivered nothing, both fail
//! here.
//!
//! What the neighbours pin and this one does not repeat: `handler_notification_fan_out` drives
//! the granularity of ONE publication's decision under a fixed policy set and the fan-out to
//! several queue policies, `handler_sdk_call_execution_role_grant` the no-grant/grant boundary
//! for two services under separate functions, and `execution_role_grant_withdrawal` the
//! withdrawal of a grant from the session credential directly, with no handler run. What only
//! this suite reaches is one LIVE function's client surviving a grant that arrives between two
//! invocations.
//!
//! Nothing here sleeps for a delivery: it polls the queue until the notifications it waits for
//! are there, and the "nothing else arrived" read is made at an instant by which a delivery owed
//! to the refused publication — made BEFORE the last admitted one — would already have arrived.
//!
//! The suite RUNS a handler, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]) and makes exactly TWO
//! invocations: one per phase, because an invocation of its own costs this target a share of the
//! run budget every other invocation suite draws on.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoding, the XML element reader and the IAM call every suite in this
// target writes a role with, owned by the suite that first needed them rather than copied.
use crate::event_source_mapping_role_access::{element, form_encode};
use crate::event_source_mappings::{await_function_active, call, call_sqs, router_on};
// The one .zip writer this target packages a running handler with.
use crate::synchronous_invoke_run::package;

/// The Region every request below is signed for, and so the Region the topics and the queue
/// belong to.
const REGION: &str = "us-east-1";

/// The account every ARN below names, which is the account this emulator serves.
const ACCOUNT: &str = "000000000000";

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The execution role the function runs as, the function's own name, and the names of the two
/// documents the grant arrives in: one inline on the role, one customer-managed and attached.
const ROLE_NAME: &str = "lambda-grant-propagation-publisher";
const FUNCTION: &str = "grant-propagation-caller";
const INLINE_POLICY: &str = "publish-grant-inline";
const MANAGED_POLICY: &str = "publish-grant-managed";

/// The AWS-managed policy a Lambda execution role is created with: it grants the log writes a
/// function makes and nothing else, so a role carrying only it has no grant on Amazon SNS.
const BASIC_EXECUTION_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole";

/// The topic the INLINE document's statement names exactly.
const INLINE_TOPIC: &str = "grant-propagation-exact";

/// The topic the attached MANAGED document covers by resource pattern, and the pattern's prefix:
/// every topic of this account and Region whose name begins this way.
const MANAGED_TOPIC: &str = "grant-propagation-family-orders";
const MANAGED_TOPIC_PREFIX: &str = "grant-propagation-family-";

/// A topic no document ever covers, so the publication to it is refused in BOTH phases: a grant
/// that arrived for other topics is not a grant on this one.
const UNGRANTED_TOPIC: &str = "grant-propagation-never";

/// The queue subscribed to all three topics, whose policy admits this service unconditionally —
/// so what it holds is decided by Amazon SNS's own decision on the publication and nothing else.
const SINK_QUEUE: &str = "grant-propagation-sink";

/// The `Type` member every notification envelope declares.
const NOTIFICATION_TYPE: &str = "Notification";

/// The error code Amazon SNS refuses a call the policies in force do not permit with:
/// `AuthorizationErrorException`'s `awsQueryError` code (`smithy_query(query="shape",
/// shape="AuthorizationErrorException")`).
const AUTHORIZATION_ERROR: &str = "AuthorizationError";

/// The status that refusal carries: the `@httpError` of that same shape.
const AUTHORIZATION_ERROR_STATUS: u64 = 403;

/// The exception class an AWS SDK raises a SERVICE's refusal as — as opposed to the class it
/// raises when it can build no client for the token at all, which is what this suite's phase one
/// must never see.
const SERVICE_REFUSAL_CLASS: &str = "ClientError";

/// How long a round trip through another service is waited for: the condition is looked at
/// again until it holds or this budget is spent, so nothing here is timed.
const CLOCK_BUDGET: Duration = Duration::from_secs(60);

/// How long between two looks.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// Lambda, Amazon SNS, Amazon SQS, IAM and the token service over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them: the handler's SDK call is dispatched back into this
/// same router, so the topic it publishes to is the topic this suite subscribed.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    router.register_service(
        service_com_amazonaws_sns_amazonsimplenotificationservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
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

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. Nothing here is signed: the suite's own calls present the account's own published
/// credential.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEMULATOR/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One awsQuery call to Amazon SNS that must be served, as the XML it carried.
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
        .header("authorization", authorization("sns"))
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

/// One Amazon SQS call that must be served.
async fn sqs(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, parsed) = call_sqs(router, operation, body).await;
    assert_eq!(
        status, 200,
        "setup: expected HTTP 200 from SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One awsQuery call to IAM that must be served, as the XML it carried.
async fn iam(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let (status, text) = crate::event_source_mapping_role_access::iam(router, action, params).await;
    assert_eq!(
        status, 200,
        "setup: expected HTTP 200 from IAM {action}, got {status} / {text}"
    );
    text
}

/// The ARN of a topic of this account in this Region.
fn topic_arn(name: &str) -> String {
    format!("arn:aws:sns:{REGION}:{ACCOUNT}:{name}")
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

/// A document stating one `sns:Publish` statement over `resource`.
fn publish_document(resource: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": "sns:Publish",
            "Resource": resource,
        }],
    })
    .to_string()
}

/// The queue policy AWS documents for a queue subscribed to a topic: the service principal
/// `sns.amazonaws.com`, allowed `sqs:SendMessage` on that queue.
fn grant_to_sns(queue_arn: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowSNSDelivery",
            "Effect": "Allow",
            "Principal": {"Service": "sns.amazonaws.com"},
            "Action": "sqs:SendMessage",
            "Resource": queue_arn,
        }],
    })
    .to_string()
}

/// One publication the handler makes, in both phases: the case, the topic it names, the text it
/// carries, and whether a document written between the two phases covers it.
struct Row {
    case: &'static str,
    topic: String,
    message: String,
    granted_in_phase_two: bool,
}

/// The publications one run makes, in the order it makes them.
///
/// The ungranted row sits BETWEEN the two admitted ones so that, once the LAST admitted
/// notification is on the queue, any delivery owed to the refused publication would already
/// have arrived — which is what makes "nothing else arrived" an assertion about the condition
/// rather than about a span of the clock.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a topic an inline role policy names exactly, publishing one plain line",
            topic: topic_arn(INLINE_TOPIC),
            message: "from-lambda-sdk".to_string(),
            granted_in_phase_two: true,
        },
        Row {
            case: "a topic no document covers, publishing text outside ASCII",
            topic: topic_arn(UNGRANTED_TOPIC),
            message: "présentation ☃ 日本語 — ok".to_string(),
            granted_in_phase_two: false,
        },
        Row {
            case: "a topic an attached managed policy covers by resource pattern, publishing a \
                   JSON document as text",
            topic: topic_arn(MANAGED_TOPIC),
            message: json!({"orderId": "A-1", "items": [1, 2, 3]}).to_string(),
            granted_in_phase_two: true,
        },
    ]
}

/// The handler both invocations run: it publishes what the event names, in order, and reports
/// what the SDK gave it for each one — the answer, or the refusal's own class, code, message and
/// status.
///
/// The client is built at MODULE scope, the way a deployment package holding only the handler
/// module builds one, so a client the SDK could not build for the token at all would fail the
/// invocation itself rather than one call — and the client the second invocation may use is the
/// one the first already built.
const HANDLER_SOURCE: &str = r#"
import boto3
import botocore.exceptions

CLIENT = boto3.client("sns")


def lambda_handler(event, context):
    results = []
    for asked in event["calls"]:
        try:
            answered = CLIENT.publish(
                TopicArn=asked["topic"], Message=asked["message"]
            )
        except botocore.exceptions.ClientError as error:
            results.append(
                {
                    "ok": False,
                    "raised": "ClientError",
                    "code": error.response["Error"]["Code"],
                    "message": error.response["Error"].get("Message"),
                    "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
                }
            )
        except Exception as error:
            results.append(
                {
                    "ok": False,
                    "raised": type(error).__name__,
                    "sentence": str(error),
                }
            )
        else:
            results.append(
                {
                    "ok": True,
                    "status": answered["ResponseMetadata"]["HTTPStatusCode"],
                    "id": answered["MessageId"],
                }
            )
    return {"results": results}
"#;

/// Invoke the function with the publications `rows` names and answer one result per row,
/// asserting the invocation itself answered 200 with no function error — the handler reports a
/// refusal in its own payload, so a function error here is the crash a client-construction
/// failure would produce.
async fn phase(router: &MultiServiceRouter, case: &str, rows: &[Row]) -> Vec<Value> {
    let calls: Vec<Value> = rows
        .iter()
        .map(|row| json!({ "topic": row.topic, "message": row.message }))
        .collect();
    let answered = call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{FUNCTION}/invocations"),
        &json!({ "calls": calls }),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected HTTP 200 from Invoke, got {} ({})",
        answered.status,
        answered.text()
    );
    let signal = answered.header("x-amz-function-error");
    assert_eq!(
        signal, None,
        "case {case:?}: expected no FunctionError, got {signal:?} with {}",
        answered.text()
    );
    let payload = answered.json();
    let results = payload["results"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        results.len(),
        rows.len(),
        "case {case:?}: expected one result per publication, got {payload}"
    );
    results
}

/// Assert `result` is the SERVICE's own authorization refusal: raised as the class an SDK raises
/// a service's answer as, carrying that service's own code, status and sentence.
fn assert_service_refusal(case: &str, result: &Value) {
    assert_eq!(
        result["ok"],
        json!(false),
        "case {case:?}: expected the publication to be refused, got {result}"
    );
    assert_eq!(
        result["raised"],
        json!(SERVICE_REFUSAL_CLASS),
        "case {case:?}: expected the SDK to raise the serving service's refusal, got {result}"
    );
    assert_eq!(
        result["code"],
        json!(AUTHORIZATION_ERROR),
        "case {case:?}: expected this service's own authorization code, got {result}"
    );
    assert_eq!(
        result["status"],
        json!(AUTHORIZATION_ERROR_STATUS),
        "case {case:?}: expected that refusal's own status, got {result}"
    );
    assert!(
        result["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty()),
        "case {case:?}: expected the service's own sentence beside the code, got {result}"
    );
}

/// The notification envelopes the queue at `url` has been handed, keyed by the `MessageId` each
/// carries, once `expected` of them have arrived — looked at again until they have or the budget
/// is spent, so what is waited for is the CONDITION and not a span.
async fn envelopes(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    expected: usize,
) -> BTreeMap<String, Value> {
    let deadline = Instant::now() + CLOCK_BUDGET;
    let mut held: BTreeMap<String, Value> = BTreeMap::new();
    loop {
        let received = sqs(
            router,
            "ReceiveMessage",
            &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
        )
        .await;
        for message in received["Messages"].as_array().cloned().unwrap_or_default() {
            let body = message["Body"].as_str().unwrap_or_default().to_string();
            let envelope: Value = serde_json::from_str(&body).unwrap_or_else(|error| {
                panic!("case {case:?}: expected a JSON envelope, got {error}: {body}")
            });
            let id = envelope["MessageId"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected a MessageId in the envelope, got {envelope}")
                })
                .to_string();
            held.insert(id, envelope);
        }
        if held.len() >= expected {
            return held;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {expected} notification(s) on the subscribed queue, got {} \
             ({:?})",
            held.len(),
            held.keys().collect::<Vec<_>>()
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Create the sink queue, admit this service on it, subscribe it to every topic in `topics`, and
/// answer its url.
async fn subscribed_queue(router: &MultiServiceRouter, topics: &[String]) -> String {
    let created = sqs(router, "CreateQueue", &json!({ "QueueName": SINK_QUEUE })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: no QueueUrl in {created}"))
        .to_string();
    let attributes = sqs(
        router,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: no QueueArn in {attributes}"))
        .to_string();
    sqs(
        router,
        "SetQueueAttributes",
        &json!({
            "QueueUrl": url,
            "Attributes": { "Policy": grant_to_sns(&arn) },
        }),
    )
    .await;
    for topic in topics {
        let answered = sns(
            router,
            "Subscribe",
            &[
                ("TopicArn", topic),
                ("Protocol", "sqs"),
                ("Endpoint", &arn),
                ("ReturnSubscriptionArn", "true"),
            ],
        )
        .await;
        let subscription = element(&answered, "SubscriptionArn");
        assert!(
            subscription.starts_with(topic.as_str()),
            "setup: expected the subscription ARN to extend {topic}, got {subscription}"
        );
    }
    url
}

/// RULE: the AWS SDK a running handler imports builds a client for every service the runtime
/// reaches, so each call it makes is decided by the serving service against the policies in
/// force on the function's execution role at that instant — refused with that service's own
/// authorization code while no statement covers it, and served as soon as one does, whichever
/// document carries the statement and however it names the resource.
#[tokio::test]
async fn a_grant_written_between_two_invocations_decides_the_handlers_next_publication() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let router = router();

    // The role the function runs as, written with IAM's own operations, carrying only the
    // managed policy a Lambda execution role is created with — so it holds no grant on this
    // service when the first invocation runs.
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

    // The three topics, each created where a client creates one, so every refusal below is about
    // the grant and not about whether the topic exists.
    for name in [INLINE_TOPIC, MANAGED_TOPIC, UNGRANTED_TOPIC] {
        let created = sns(&router, "CreateTopic", &[("Name", name)]).await;
        let arn = element(&created, "TopicArn");
        assert_eq!(
            arn,
            topic_arn(name),
            "setup: expected the topic ARN of this account and Region, got {arn}"
        );
    }

    let rows = rows();
    let topics: Vec<String> = rows.iter().map(|row| row.topic.clone()).collect();
    let sink = subscribed_queue(&router, &topics).await;

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

    // PHASE ONE: no statement covers any of the publications. Each one is the SERVICE's own
    // refusal — so the client was built and the call was made, which is what tells "refused"
    // apart from "no such client".
    let before = phase(&router, "before the grant is written", &rows).await;
    for (row, result) in rows.iter().zip(&before) {
        assert_service_refusal(row.case, result);
    }

    // The grant arrives, in two documents of its own: one inline on the role naming its topic
    // exactly, and one customer-managed policy, attached to the role, covering a family of
    // topics by resource pattern.
    iam(
        &router,
        "PutRolePolicy",
        &[
            ("RoleName", ROLE_NAME),
            ("PolicyName", INLINE_POLICY),
            ("PolicyDocument", &publish_document(&topic_arn(INLINE_TOPIC))),
        ],
    )
    .await;
    let managed = iam(
        &router,
        "CreatePolicy",
        &[
            ("PolicyName", MANAGED_POLICY),
            (
                "PolicyDocument",
                &publish_document(&format!(
                    "arn:aws:sns:{REGION}:{ACCOUNT}:{MANAGED_TOPIC_PREFIX}*"
                )),
            ),
        ],
    )
    .await;
    iam(
        &router,
        "AttachRolePolicy",
        &[
            ("RoleName", ROLE_NAME),
            ("PolicyArn", &element(&managed, "Arn")),
        ],
    )
    .await;

    // PHASE TWO: the same calls through the same handler. Each publication a document now covers
    // is served and mints a MessageId of its own; the one no document covers is refused exactly
    // as before, so what the grant admitted is the statement's own resource and nothing beside
    // it.
    let after = phase(&router, "after the grant is written", &rows).await;
    let mut minted: Vec<(&Row, String)> = Vec::new();
    for (row, result) in rows.iter().zip(&after) {
        if !row.granted_in_phase_two {
            assert_service_refusal(row.case, result);
            continue;
        }
        assert_eq!(
            result["ok"],
            json!(true),
            "case {:?}: expected the publication to be served once the grant is written, got \
             {result}",
            row.case
        );
        assert_eq!(
            result["status"],
            json!(200),
            "case {:?}: expected the SDK to report HTTP 200, got {result}",
            row.case
        );
        let id = result["id"].as_str().unwrap_or_default().to_string();
        assert!(
            !id.is_empty(),
            "case {:?}: expected the service to mint a MessageId, got {result}",
            row.case
        );
        assert!(
            !minted.iter().any(|(_row, seen)| *seen == id),
            "case {:?}: expected a MessageId of its own, got the one already minted: {id}",
            row.case
        );
        minted.push((row, id));
    }
    assert_eq!(
        minted.len(),
        rows.iter().filter(|row| row.granted_in_phase_two).count(),
        "case \"after the grant is written\": expected every covered publication to be served, \
         got {after:?}"
    );

    // The other half: what the subscribed queue holds. Every publication the decision admitted
    // arrived, carrying its own text byte for byte and naming the topic it was published to.
    let case = "the queue subscribed to all three topics";
    let held = envelopes(&router, case, &sink, minted.len()).await;
    for (row, id) in &minted {
        let envelope = held.get(id).unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected the publication {:?} ({id}) on the queue, got {:?}",
                row.case,
                held.keys().collect::<Vec<_>>()
            )
        });
        assert_eq!(
            envelope["Type"],
            json!(NOTIFICATION_TYPE),
            "case {case:?}: expected a notification envelope for {:?}, got {envelope}",
            row.case
        );
        assert_eq!(
            envelope["TopicArn"],
            json!(row.topic),
            "case {case:?}: expected the envelope to name the published topic for {:?}, got \
             {envelope}",
            row.case
        );
        assert_eq!(
            envelope["Message"].as_str(),
            Some(row.message.as_str()),
            "case {case:?}: expected the published message verbatim for {:?}, got {:?}",
            row.case,
            envelope["Message"]
        );
    }

    // And nothing else: the refused publications left no delivery behind. The refusal of phase
    // two was made BEFORE the last admitted publication, which is already on the queue, so a
    // delivery owed to it would be here too.
    assert_eq!(
        held.len(),
        minted.len(),
        "case {case:?}: expected only the admitted publications on the queue, got {:?}",
        held.values().collect::<Vec<_>>()
    );
    let refused_texts: Vec<&str> = rows
        .iter()
        .filter(|row| !row.granted_in_phase_two)
        .map(|row| row.message.as_str())
        .collect();
    for envelope in held.values() {
        let carried = envelope["Message"].as_str().unwrap_or_default();
        assert!(
            !refused_texts.contains(&carried),
            "case {case:?}: expected no refused publication on the queue, got {envelope}"
        );
    }
}
