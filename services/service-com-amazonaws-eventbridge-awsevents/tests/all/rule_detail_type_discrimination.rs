// SPDX-License-Identifier: CC0-1.0

//! A rule whose `EventPattern` PINS the envelope's `detail-type`, and what each published
//! entry reaches its Lambda target with — end to end over the wire, with the services that
//! target's own handler calls mounted beside EventBridge on one engine.
//!
//! The RULE pinned here: **every entry a publication ACCEPTS (`FailedEntryCount` 0, which is
//! this operation's only promise — `PutEvents` models no client-side refusal channel) is
//! matched against the `EventPattern` of each ENABLED rule of the bus it names, and a pattern
//! matches only when EVERY member it names matches the envelope: the `source`, the
//! `detail-type` — compared as the whole string, so neither a differently cased nor a longer
//! one matches, and an entry carrying none at all matches no value list — and each member
//! nested inside `detail`, however deep, while an envelope member the pattern does NOT name
//! constrains nothing. Each target of a matching rule is then handed that entry once, under
//! the documents in force on it, carrying the whole event envelope with `source`,
//! `detail-type` and `detail` exactly as published; an entry differing on any one named
//! member is handed to nothing.**
//!
//! Both legs are asserted in ONE run, in the order a client writes them, because either leg
//! alone is uninformative: a negative leg ("the differing detail-type delivers nothing") is
//! satisfied for free by a dispatcher that delivers nothing at all, so this suite publishes
//! every NON-matching entry first, watches the target's own sink stay empty, then publishes
//! the matching ones and waits for exactly their deliveries. The second observation is what
//! makes the first one mean something — a pattern that is too broad and one that is correctly
//! narrow are indistinguishable until something is known to be delivered.
//!
//! The rows differ along what the rule generalises over: WHICH named member of the pattern the
//! entry differs on (the `detail-type`, the `source`, a member of `detail`, a member nested
//! one level deeper inside `detail`), HOW it differs (another value, the same value in another
//! case, the pinned value extended, the pinned value padded, the containing object absent),
//! and
//! whether the entry carries members beyond the ones the pattern names. The recorded case is
//! the pair "every pattern member matches" / "the detail-type differs, every other member
//! matches".
//!
//! Every observation is one a client makes: the function's own handler relays the envelope it
//! was handed to an Amazon SQS queue, and the suite reads that queue through `ReceiveMessage`.

use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The one percent-encoder the suites of this crate share, for the awsQuery body an IAM call
// carries.
use crate::target_dispatch::form_encode;

/// The Region every request below is signed for, and so the Region the queue, the function and
/// the rule belong to.
const REGION: &str = "us-west-2";

/// The `source` the rule's pattern names, and one it does not.
const SOURCE: &str = "com.demo.cdn.automation";
/// See [`SOURCE`].
const OTHER_SOURCE: &str = "com.demo.cdn.elsewhere";

/// The `detail-type` the rule's pattern pins, and the four strings a row differs from it by:
/// another detail type altogether, the same words in another case, the pinned value extended,
/// and the pinned value padded. All of them are values a client publishes, not values this
/// emulator computes.
const DETAIL_TYPE: &str = "AWS API Call via CloudTrail";
/// See [`DETAIL_TYPE`].
const OTHER_DETAIL_TYPE: &str = "CDN Distribution State-Change";
/// See [`DETAIL_TYPE`].
const RECASED_DETAIL_TYPE: &str = "aws api call via cloudtrail";
/// See [`DETAIL_TYPE`].
const EXTENDED_DETAIL_TYPE: &str = "AWS API Call via CloudTrail (replayed)";
/// See [`DETAIL_TYPE`].
const PADDED_DETAIL_TYPE: &str = " AWS API Call via CloudTrail ";

/// The member of `detail` the pattern names one level in, and the value it pins; plus a value
/// a row differs from it by.
const EVENT_NAME_MEMBER: &str = "eventName";
/// See [`EVENT_NAME_MEMBER`].
const EVENT_NAME: &str = "CreateInvalidation";
/// See [`EVENT_NAME_MEMBER`].
const OTHER_EVENT_NAME: &str = "CreateDistribution";

/// The object of `detail` the pattern reaches one level DEEPER into, the member it pins there
/// and the value it pins; plus a value a row differs from it by.
const PARAMETERS_MEMBER: &str = "requestParameters";
/// See [`PARAMETERS_MEMBER`].
const RESOURCE_MEMBER: &str = "distributionId";
/// See [`PARAMETERS_MEMBER`].
const RESOURCE_ID: &str = "E2ABCDEF012345";
/// See [`PARAMETERS_MEMBER`].
const OTHER_RESOURCE_ID: &str = "E3FEDCBA543210";

/// The member of `detail` the pattern does NOT name, which every entry carries so a delivery
/// is read back against the entry it came from.
const MARKER: &str = "marker";

/// The principal EventBridge invokes a resource-policy-governed target as, and the action such
/// an invocation performs: "for Lambda functions, Amazon SNS topics and Amazon SQS queues,
/// EventBridge relies on resource-based policies" (`PutTargets`).
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
/// See [`EVENTS_PRINCIPAL`].
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The principal the function's execution role is assumed by.
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";

/// The module and attribute the deployment package carries its handler as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// How long one invocation is given, as the function's own `Timeout`: well past what the
/// handler needs on any host.
const FUNCTION_TIMEOUT: u64 = 60;

/// The environment variable the handler reads the witness queue's name from.
const WITNESS_VARIABLE: &str = "WITNESS_QUEUE";

/// How long each observation of work the clock owes — the function becoming `Active`, an
/// asynchronous invocation running, the handler's own relay arriving — waits before giving up,
/// and how long between two looks. The deadline is generous: what is under test is that the
/// work happens at all, never how fast this host gets to it.
const BUDGET: Duration = Duration::from_secs(60);
/// See [`BUDGET`].
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// How long an observation that must stay EMPTY keeps looking before concluding nothing came.
/// Nothing rests on this window alone: the run's own positive leg, asserted afterwards, is
/// what shows the dispatcher was live while it was open.
const QUIET_BUDGET: Duration = Duration::from_secs(3);

/// The queue name and the function and rule names this suite's resources carry.
const WITNESS_QUEUE: &str = "detail-type-witness";
/// See [`WITNESS_QUEUE`].
const FUNCTION: &str = "detail-type-relay";
/// See [`WITNESS_QUEUE`].
const RULE: &str = "detail-type-rule";
/// See [`WITNESS_QUEUE`].
const EXECUTION_ROLE: &str = "detail-type-relay-exec";

/// The handler the function target deploys: it relays the envelope EventBridge handed it to the
/// witness queue, which is where a client observes that the invocation really ran and what it
/// was handed. A payload carrying no `detail-type` is no envelope — that is the direct warm-up
/// invocation a client makes before wiring the rule — so it relays nothing and says so.
const RELAY_SOURCE: &str = r#"
import json
import os

import boto3


def lambda_handler(event, context):
    if "detail-type" not in event:
        return {"relayed": False}
    client = boto3.client("sqs")
    url = client.get_queue_url(QueueName=os.environ["WITNESS_QUEUE"])["QueueUrl"]
    client.send_message(QueueUrl=url, MessageBody=json.dumps(event, sort_keys=True))
    return {"relayed": True}
"#;

/// EventBridge, Lambda, Amazon SQS, IAM and the token service over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them: the handler's own SDK call is dispatched back into this
/// same router, so the queue it writes is the queue this suite reads.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite signs nothing: the `Authorization` header carries only the credential scope,
    // which is where a service reads the Region and the caller's access key id from. The
    // handler's OWN call is signed by the runtime with the credential it was given.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
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
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One `X-Amz-Target` RPC call, as its status and parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`) that must answer 200.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "events", "AWSEvents", "1.1", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from EventBridge {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// One Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`) that must answer 200.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "sqs", "AmazonSQS", "1.0", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One Lambda `restJson1` call, as its status, its headers and its parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &Value,
) -> (u16, http::HeaderMap, Value) {
    let payload = if body.is_null() {
        bytes::Bytes::new()
    } else {
        bytes::Bytes::from(body.to_string())
    };
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(payload)
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, headers, parsed)
}

/// One awsQuery call to IAM, as the account's own credential, which must be served.
async fn iam(router: &MultiServiceRouter, case: &str, action: &str, params: &[(&str, String)]) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("iam"))
        .body(bytes::Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from IAM {action}, got {status} with {answered}"
    );
}

/// A .zip deployment package holding [`RELAY_SOURCE`] as `lambda_function.py`, written in
/// memory the way a caller uploads one and base64-encoded as `FunctionCode$ZipFile` carries it.
fn package() -> String {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("lambda_function.py", options)
            .expect("the entry starts");
        writer
            .write_all(RELAY_SOURCE.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    base64::engine::general_purpose::STANDARD.encode(buffer.into_inner())
}

/// The pattern the one rule of this suite carries: the `source`, the `detail-type` and two
/// members of `detail`, one of them nested a level deeper.
fn pattern() -> String {
    json!({
        "source": [SOURCE],
        "detail-type": [DETAIL_TYPE],
        "detail": {
            EVENT_NAME_MEMBER: [EVENT_NAME],
            PARAMETERS_MEMBER: { RESOURCE_MEMBER: [RESOURCE_ID] },
        },
    })
    .to_string()
}

/// A `detail` carrying `marker`, the pinned `eventName` and the pinned nested resource id —
/// the document every row starts from before differing on one member.
fn matching_detail(marker: &str) -> Value {
    json!({
        MARKER: marker,
        EVENT_NAME_MEMBER: EVENT_NAME,
        PARAMETERS_MEMBER: { RESOURCE_MEMBER: RESOURCE_ID },
    })
}

/// One published entry: what it carries, and whether the rule's pattern matches it.
struct Row {
    case: &'static str,
    /// The value of the one `detail` member the pattern does not name, which tells the rows
    /// apart wherever a delivery is read back.
    marker: &'static str,
    source: &'static str,
    /// The entry's `DetailType`, which `PutEvents` requires of every entry.
    detail_type: &'static str,
    detail: Value,
    /// Whether the pattern matches this entry, and so whether the target is handed it.
    delivered: bool,
}

/// Every entry this suite publishes, the matching ones and the differing ones together.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "every member the pattern names matches",
            marker: "match-plain",
            source: SOURCE,
            detail_type: DETAIL_TYPE,
            detail: matching_detail("match-plain"),
            delivered: true,
        },
        Row {
            case: "every member the pattern names matches, and the detail carries members \
                   beyond them, of every JSON type",
            marker: "match-superset",
            source: SOURCE,
            detail_type: DETAIL_TYPE,
            detail: {
                let mut detail = matching_detail("match-superset");
                detail[PARAMETERS_MEMBER]["callerReference"] = json!("demo-reference");
                detail[PARAMETERS_MEMBER]["paths"] = json!({ "quantity": 2, "items": ["/a", "/b"] });
                detail["attempt"] = json!(3);
                detail["ratio"] = json!(0.5);
                detail["dryRun"] = json!(false);
                detail["cause"] = Value::Null;
                detail
            },
            delivered: true,
        },
        Row {
            case: "the detail-type differs, every other member the pattern names matches",
            marker: "other-detail-type",
            source: SOURCE,
            detail_type: OTHER_DETAIL_TYPE,
            detail: matching_detail("other-detail-type"),
            delivered: false,
        },
        Row {
            case: "the detail-type is the pinned one in another case",
            marker: "recased-detail-type",
            source: SOURCE,
            detail_type: RECASED_DETAIL_TYPE,
            detail: matching_detail("recased-detail-type"),
            delivered: false,
        },
        Row {
            case: "the detail-type carries the pinned one as a prefix of a longer string",
            marker: "extended-detail-type",
            source: SOURCE,
            detail_type: EXTENDED_DETAIL_TYPE,
            detail: matching_detail("extended-detail-type"),
            delivered: false,
        },
        Row {
            case: "the detail-type is the pinned one padded with whitespace",
            marker: "padded-detail-type",
            source: SOURCE,
            detail_type: PADDED_DETAIL_TYPE,
            detail: matching_detail("padded-detail-type"),
            delivered: false,
        },
        Row {
            case: "the detail-type matches and the detail's own eventName differs",
            marker: "other-event-name",
            source: SOURCE,
            detail_type: DETAIL_TYPE,
            detail: {
                let mut detail = matching_detail("other-event-name");
                detail[EVENT_NAME_MEMBER] = json!(OTHER_EVENT_NAME);
                detail
            },
            delivered: false,
        },
        Row {
            case: "the detail-type matches and the member nested a level deeper in the detail \
                   differs",
            marker: "other-resource-id",
            source: SOURCE,
            detail_type: DETAIL_TYPE,
            detail: {
                let mut detail = matching_detail("other-resource-id");
                detail[PARAMETERS_MEMBER][RESOURCE_MEMBER] = json!(OTHER_RESOURCE_ID);
                detail
            },
            delivered: false,
        },
        Row {
            case: "the detail-type matches and the object holding that nested member is absent",
            marker: "absent-parameters",
            source: SOURCE,
            detail_type: DETAIL_TYPE,
            detail: json!({ MARKER: "absent-parameters", EVENT_NAME_MEMBER: EVENT_NAME }),
            delivered: false,
        },
        Row {
            case: "the detail-type matches and the source differs",
            marker: "other-source",
            source: OTHER_SOURCE,
            detail_type: DETAIL_TYPE,
            detail: matching_detail("other-source"),
            delivered: false,
        },
    ]
}

/// The entry one row publishes.
fn entry(row: &Row) -> Value {
    json!({
        "Source": row.source,
        "DetailType": row.detail_type,
        "Detail": row.detail.to_string(),
    })
}

/// Publish `rows` as the entries of ONE request, assert every one of them is ACCEPTED, and
/// answer the `EventId` each was given.
async fn publish(router: &MultiServiceRouter, case: &str, rows: &[&Row]) -> Vec<String> {
    let entries: Vec<Value> = rows.iter().map(|row| entry(row)).collect();
    let published = events(router, case, "PutEvents", &json!({ "Entries": entries })).await;
    assert_eq!(
        published["FailedEntryCount"],
        json!(0),
        "case {case:?}: an accepted publication reports no failed entry, got {published}"
    );
    let ids: Vec<String> = published["Entries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| entry["EventId"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        ids.len(),
        rows.len(),
        "case {case:?}: expected one answered EventId per published entry, got {published}"
    );
    ids
}

/// Every message waiting on the queue at `url`, taken off it: each is DELETED as it is read,
/// so what a later look answers is what arrived after this one rather than a copy whose
/// visibility timeout ran out.
async fn drain(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let received = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    for message in &messages {
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                router,
                case,
                "DeleteMessage",
                &json!({ "QueueUrl": url, "ReceiptHandle": handle }),
            )
            .await;
        }
    }
    messages
}

/// Everything the queue at `url` holds once it holds `want` messages, so a delivery still in
/// flight is looked for again rather than read as missing; gives up after [`BUDGET`] so a
/// message that never arrives fails an assertion instead of hanging.
async fn messages_until(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    want: usize,
) -> Vec<Value> {
    let deadline = Instant::now() + BUDGET;
    let mut held: Vec<Value> = Vec::new();
    loop {
        held.extend(drain(router, case, url).await);
        if held.len() >= want || Instant::now() >= deadline {
            return held;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Whatever the queue at `url` holds within [`QUIET_BUDGET`], looked for again and again so a
/// delivery that did happen is seen rather than missed.
async fn whatever_arrives(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let until = Instant::now() + QUIET_BUDGET;
    let mut held: Vec<Value> = Vec::new();
    while Instant::now() < until {
        held.extend(drain(router, case, url).await);
        if !held.is_empty() {
            return held;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    held
}

/// The envelope one relayed message carries.
fn envelope(case: &str, message: &Value) -> Value {
    let body = message["Body"].as_str().unwrap_or_default();
    serde_json::from_str(body).unwrap_or_else(|error| {
        panic!("case {case:?}: a relayed message carries the event envelope, got {body} ({error})")
    })
}

/// A rule pinning the envelope's `detail-type` hands its Lambda target every entry matching
/// EVERY member of its pattern — carrying `source`, `detail-type` and `detail` as published —
/// and hands it no entry differing on one of them.
#[tokio::test]
async fn a_pattern_pinning_the_detail_type_delivers_only_the_entries_matching_every_member() {
    let case = "a rule pinning source, detail-type and two members of detail";
    let router = router();

    // The queue the function's handler relays to, which is where a client observes what the
    // invocation was handed. The handler writes it as its own execution role, so the queue
    // carries no policy for this service.
    let created = sqs(
        &router,
        case,
        "CreateQueue",
        &json!({ "QueueName": WITNESS_QUEUE }),
    )
    .await;
    let witness_url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl, got {created}"))
        .to_string();

    // The execution role the handler's own relay is judged against.
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": LAMBDA_PRINCIPAL },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    iam(
        &router,
        case,
        "CreateRole",
        &[
            ("RoleName", EXECUTION_ROLE.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ],
    )
    .await;
    let inline = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["sqs:GetQueueUrl", "sqs:SendMessage"],
            "Resource": "*",
        }],
    })
    .to_string();
    iam(
        &router,
        case,
        "PutRolePolicy",
        &[
            ("RoleName", EXECUTION_ROLE.to_string()),
            ("PolicyName", "inline".to_string()),
            ("PolicyDocument", inline),
        ],
    )
    .await;
    let role_arn = format!(
        "arn:aws:iam::{}:role/{EXECUTION_ROLE}",
        engine::emulator_account_id()
    );

    // The function, and the direct warm-up invocation a client makes before wiring the rule:
    // a payload that is no envelope relays nothing, so the warm-up leaves the witness queue
    // untouched while still showing the function and its role work.
    let function_arn = given_function(&router, case, &role_arn).await;
    let (status, headers, answered) = lambda(
        &router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/invocations"),
        &json!({ "warmup": true }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from a RequestResponse Invoke of the warm-up payload, \
         got {status} with {answered}"
    );
    assert!(
        !headers.contains_key("x-amz-function-error"),
        "case {case:?}: expected the warm-up invocation to carry no function error, got \
         {headers:?} with {answered}"
    );
    assert_eq!(
        answered,
        json!({ "relayed": false }),
        "case {case:?}: expected the warm-up invocation to relay nothing, got {answered}"
    );

    // The rule, its pattern, and the one target — admitted by the function's own
    // resource-based policy, scoped by `aws:SourceArn` to this rule.
    let written = events(
        &router,
        case,
        "PutRule",
        &json!({ "Name": RULE, "EventPattern": pattern(), "State": "ENABLED" }),
    )
    .await;
    let rule_arn = written["RuleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a RuleArn, got {written}"))
        .to_string();
    let (status, _, granted) = lambda(
        &router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/policy"),
        &json!({
            "StatementId": "AllowEventBridgeInvoke",
            "Action": INVOKE_ACTION,
            "Principal": EVENTS_PRINCIPAL,
            "SourceArn": rule_arn,
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected HTTP 201 from AddPermission, got {status} with {granted}"
    );
    let targeted = events(
        &router,
        case,
        "PutTargets",
        &json!({
            "Rule": RULE,
            "Targets": [{ "Id": "target-1", "Arn": function_arn }],
        }),
    )
    .await;
    assert_eq!(
        targeted["FailedEntryCount"],
        json!(0),
        "case {case:?}: the target must be accepted, got {targeted}"
    );

    let rows = rows();
    let differing: Vec<&Row> = rows.iter().filter(|row| !row.delivered).collect();
    let matching: Vec<&Row> = rows.iter().filter(|row| row.delivered).collect();

    // Leg one: every entry differing on one member the pattern names, published first and
    // accepted, reaches the target with nothing.
    publish(&router, case, &differing).await;
    let early = whatever_arrives(&router, case, &witness_url).await;
    assert!(
        early.is_empty(),
        "case {case:?}: expected the entries differing on a member the pattern names to be \
         handed to nothing, got {early:?}"
    );

    // Leg two: the matching entries, which make leg one mean something — each is handed to the
    // target once, carrying the envelope as published.
    let matching_ids = publish(&router, case, &matching).await;
    let relayed = messages_until(&router, case, &witness_url, matching.len()).await;
    assert_eq!(
        relayed.len(),
        matching.len(),
        "case {case:?}: expected the target to be handed each of the {} matching entries once, \
         got {relayed:?}",
        matching.len()
    );
    for (row, id) in matching.iter().zip(&matching_ids) {
        let found = relayed
            .iter()
            .map(|message| envelope(case, message))
            .find(|envelope| envelope["detail"][MARKER] == json!(row.marker));
        let Some(envelope) = found else {
            panic!(
                "case {:?}: expected the target to be handed the matching entry, got {relayed:?}",
                row.case
            );
        };
        assert_eq!(
            envelope["source"],
            json!(row.source),
            "case {:?}: expected the handed envelope to carry the published source, got \
             {envelope}",
            row.case
        );
        assert_eq!(
            envelope["detail-type"],
            json!(row.detail_type),
            "case {:?}: expected the handed envelope to carry the published detail-type, got \
             {envelope}",
            row.case
        );
        assert_eq!(
            envelope["detail"], row.detail,
            "case {:?}: expected the handed envelope to carry the published detail unchanged, \
             every value keeping its JSON type, got {envelope}",
            row.case
        );
        assert_eq!(
            envelope["id"],
            json!(id),
            "case {:?}: expected the handed envelope to carry the EventId the publication \
             answered, got {envelope}",
            row.case
        );
    }

    // The differing entries are handed to nothing LATER either: by now every delivery the
    // matching entries owed has been made, so a delivery of a differing one would be here.
    let late = whatever_arrives(&router, case, &witness_url).await;
    assert!(
        late.is_empty(),
        "case {case:?}: expected nothing further to be handed to the target, got {late:?}"
    );
    for row in &differing {
        assert!(
            !relayed
                .iter()
                .any(|message| envelope(case, message)["detail"][MARKER] == json!(row.marker)),
            "case {:?}: expected the entry differing on a member the pattern names to be handed \
             to nothing, got {relayed:?}",
            row.case
        );
    }
}

/// Create the function the rule targets, wait until Lambda reports it `Active` — "during this
/// time, you can't invoke or modify the function" (`CreateFunction`) — and answer its ARN.
async fn given_function(router: &MultiServiceRouter, case: &str, role_arn: &str) -> String {
    let (status, _, created) = lambda(
        router,
        "POST",
        "/2015-03-31/functions",
        &json!({
            "FunctionName": FUNCTION,
            "Role": role_arn,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Timeout": FUNCTION_TIMEOUT,
            "Environment": { "Variables": { WITNESS_VARIABLE: WITNESS_QUEUE } },
            "Code": { "ZipFile": package() },
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected HTTP 201 from CreateFunction, got {status} with {created}"
    );
    let arn = created["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a FunctionArn, got {created}"))
        .to_string();
    let deadline = Instant::now() + BUDGET;
    loop {
        let (_, _, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
            &Value::Null,
        )
        .await;
        if configuration["State"].as_str() == Some("Active") {
            return arn;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: Lambda never reported {FUNCTION:?} Active, last read {configuration}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}
