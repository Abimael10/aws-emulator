// SPDX-License-Identifier: CC0-1.0

//! What Amazon SQS answers a call naming a queue the CLEANUP leg of an EventBridge Pipes
//! lifecycle removed — over the WHOLE mounted service set, the one `core/router/src/lib.rs`
//! assembles for the binary.
//!
//! The RULE under test: **a request naming a queue this account no longer holds is refused HTTP
//! 400 in the `QueueDoesNotExist` class — reported beside the legacy query code the model binds
//! to it, `AWS.SimpleQueueService.NonExistentQueue` — carrying the FIXED sentence the wire
//! protocol the request arrived in words that class with, whatever operation was called and
//! however the queue was addressed (its `QueueUrl`, or the `QueueName` the one name-addressed
//! operation resolves).** On this service's `awsJson1_0` surface that sentence is "The specified
//! queue does not exist."; on its WSDL-versioned AWS Query surface it is the same statement
//! naming that surface's version. Either way the sentence is a CONSTANT — the code is what
//! identifies the condition — so nothing of the request is interpolated into it: no queue name,
//! no url, no account id, and no clause of the other surface's wording.
//!
//! Three services have to agree for that to hold on the path this suite drives: EventBridge
//! Pipes runs the pipe, IAM holds the execution role whose policies admit each stage's call, and
//! Amazon SQS holds the source and the target and decides every later call on them. What is new
//! here — and what no crate-local suite can reach — is the CLEANUP leg of a whole pipe
//! lifecycle on the production service set: the queues probed below are queues that really
//! carried a record through a RUNNING pipe a moment earlier, and were then taken away in the
//! order a client tears such a topology down (stop the pipe, delete the pipe, delete both
//! queues). That is how a client proves the teardown propagated to the data plane that was
//! serving, so the sentence it matches on has to be AWS's own.
//!
//! Overlap, stated rather than hidden: the Amazon SQS crate's `queue_does_not_exist` suite drives
//! the table of operations and absence forms against that service mounted alone, its
//! `query_compatible_error_envelope` suite pins the AWS Query surface's own sentence, and
//! `cloudcontrol_deleted_queue_data_plane_refusal` holds the same refusal after a uniform
//! control plane's delete. None of them runs a pipe over the queues first.
//!
//! The rows differ along what the rule generalises over: the ROLE the deleted queue played in the
//! pipe (its source, its target), the FORM of the absence (a queue deleted after it was serving,
//! and a name the account never created), the ADDRESSING form (the `QueueName` `GetQueueUrl`
//! resolves, and the `QueueUrl` every other operation takes), the OPERATION called, and the
//! SURFACE the client speaks (the `awsJson1_0` one every current SDK sends, and the AWS Query
//! form wire).

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The whole emulator over one engine, and the one spelling of a SigV4 `Authorization` in this
// test target: a sibling suite declares both `pub(crate)`, so neither is written twice.
use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{authorization, router};
use crate::query_wire::{element, form_encode};

/// The Region the whole wiring below lives in, which is what each handler reads its Region from
/// and which Region the queue ARNs a pipe names its source and target by live in.
const REGION: &str = "us-west-2";

/// Amazon SQS's own sentence for a queue the account does not hold, on the `awsJson1_0` surface
/// every modern SDK speaks — written out here rather than read off the service, so what this
/// suite asserts is AWS's text and not the emulator's idea of it.
///
/// RECORDED: a `GetQueueUrl` for the name of a queue that was deleted, and a `SendMessage` on
/// that queue's url, are refused HTTP 400 `AWS.SimpleQueueService.NonExistentQueue` with exactly
/// this text. The model (`smithy_query(query="shape", shape="QueueDoesNotExist",
/// model_path="services/service-com-amazonaws-sqs-amazonsqs/smithy-model/com-amazonaws-sqs-amazonsqs.json")`)
/// fixes no message text, so the served sentence is the requirement; it names no queue, so it has
/// no variable part at all.
const AWS_SENTENCE: &str = "The specified queue does not exist.";

/// The sentence the WSDL-versioned AWS Query surface of the same API words the same class with,
/// which the JSON surface above never serves. One API, two protocols, two sentences for one
/// class, with the same code and status on both.
const QUERY_API_SENTENCE: &str = "The specified queue does not exist for this wsdl version.";

/// The class the refusal names, as the `awsJson1_0` discriminator carries it, and the status the
/// model binds to it (`httpError: 400`).
const QUEUE_DOES_NOT_EXIST: &str = "QueueDoesNotExist";
const REFUSAL_STATUS: u16 = 400;

/// The legacy query code the model's `awsQueryError` trait binds to that class — what a query
/// client reads as `Error.Code` — and the `code;fault` report an `awsQueryCompatible` service
/// puts beside the class on the JSON surface, in the header below.
const NON_EXISTENT_QUEUE: &str = "AWS.SimpleQueueService.NonExistentQueue";
const NON_EXISTENT_QUEUE_REPORT: &str = "AWS.SimpleQueueService.NonExistentQueue;Sender";
const QUERY_ERROR_HEADER: &str = "x-amzn-query-error";

/// The API version an AWS Query client of this endpoint names in every request
/// (`smithy_query(query="service_info",
/// model_path="services/service-com-amazonaws-sqs-amazonsqs/smithy-model/com-amazonaws-sqs-amazonsqs.json")`:
/// version `2012-11-05`).
const QUERY_API_VERSION: &str = "2012-11-05";

/// The media types the two Amazon SQS surfaces carry their requests in, and the one EventBridge
/// Pipes carries its `restJson1` body in.
const JSON_CONTENT_TYPE: &str = "application/x-amz-json-1.0";
const FORM_CONTENT_TYPE: &str = "application/x-www-form-urlencoded";
const REST_CONTENT_TYPE: &str = "application/json";

/// How long work a pipe or a queue does on its own is given before the suite calls it undone, and
/// how often the thing waited on is looked at again. Far above the span a pipe takes to reach
/// RUNNING and the data plane's own poll interval, so a leg fails only when the work was not done
/// at all rather than because the host was busy.
const BUDGET: Duration = Duration::from_secs(60);
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// The visibility timeout the source queue is created with: a record the pipe took is hidden from
/// every other consumer for this long, so the delivery the suite waits for is the pipe's.
const SOURCE_VISIBILITY: &str = "60";

/// The pipe, the queues it moves records between, the role it runs as, and the name the account
/// never creates a queue under — one naming scheme, so every value a refusal must not name is
/// known to the assertions.
const PIPE: &str = "pipes-e2e-cleanup";
const SOURCE_QUEUE: &str = "pipes-e2e-cleanup-src";
const TARGET_QUEUE: &str = "pipes-e2e-cleanup-tgt";
const ROLE: &str = "pipes-e2e-cleanup-role";
const NEVER_CREATED_QUEUE: &str = "pipes-e2e-cleanup-never-created";

/// The record the pipe moves, so the queues probed after the teardown are queues that really
/// served a moment earlier.
const RECORD: &str = "the record the pipe moves";

/// RULE: see the module docs — once the cleanup leg of a pipe lifecycle has taken the queues
/// away, every later call naming either of them is refused HTTP 400
/// `AWS.SimpleQueueService.NonExistentQueue` carrying the fixed sentence of the surface it
/// arrived on, with nothing of the request in it.
#[tokio::test]
async fn the_queues_a_finished_pipe_moved_records_between_are_refused_as_absent() {
    let case = "pipes-lifecycle-cleanup";
    let router = router();

    // The topology a client stands up: two queues, a role with exactly the calls a queue-to-queue
    // pipe's stages make, and a pipe between them, waited on until it reports itself RUNNING.
    let (source_url, source_arn) = given_queue(&router, case, SOURCE_QUEUE, true).await;
    let (target_url, target_arn) = given_queue(&router, case, TARGET_QUEUE, false).await;
    let role_arn = given_role(&router, case, ROLE, &source_arn, &target_arn).await;
    given_running_pipe(&router, case, &source_arn, &target_arn, &role_arn).await;

    // The lifecycle itself: one record produced to the source arrives at the target, as the
    // pipe's own doing.
    send(&router, case, &source_url, RECORD).await;
    let delivered = wait_for_delivery(&router, case, &target_url).await;
    let envelope: Value = serde_json::from_str(&delivered).unwrap_or(Value::Null);
    let moved = envelope["body"].as_str().unwrap_or(delivered.as_str());
    assert_eq!(
        moved, RECORD,
        "case {case:?}: expected the pipe to deliver {RECORD:?} to its target, got \
         {delivered:?}"
    );

    // The cleanup leg, in the order a client tears such a topology down.
    stop_and_delete_pipe(&router, case).await;
    for url in [&source_url, &target_url] {
        ok(&router, case, "DeleteQueue", &json!({ "QueueUrl": url })).await;
    }
    wait_until_absent(&router, case, SOURCE_QUEUE).await;
    wait_until_absent(&router, case, TARGET_QUEUE).await;

    // The account the queues lived in, read off the ARN the pipe named its source by: one more
    // value no refusal may hand back to the caller.
    let account = source_arn
        .split(':')
        .nth(4)
        .unwrap_or_else(|| panic!("case {case:?}: no account in the queue ARN {source_arn:?}"))
        .to_string();

    // The absence forms: the two queues the lifecycle took away, and a name the account never
    // created at all — the condition is the absence, and nothing of the queue's history is part
    // of it.
    let forms = [
        ("the deleted source queue", SOURCE_QUEUE, source_url.clone()),
        ("the deleted target queue", TARGET_QUEUE, target_url.clone()),
        (
            "a name the account never created",
            NEVER_CREATED_QUEUE,
            sibling_url(&source_url, NEVER_CREATED_QUEUE),
        ),
    ];

    for (form, name, url) in &forms {
        for (operation, request) in requests_naming(name, url) {
            let answered = sqs_json(&router, &operation, &request).await;
            assert_json_absence(
                form,
                &operation,
                &[name, url.as_str(), account.as_str()],
                answered,
            );
        }
    }

    // The other surface of the same API keeps its own sentence for the same class, with the same
    // code and status: the two wordings are per-protocol constants, and neither leaks into the
    // other.
    let query_rows = [
        ("GetQueueUrl", vec![("QueueName", SOURCE_QUEUE.to_string())]),
        (
            "SendMessage",
            vec![
                ("QueueUrl", source_url.clone()),
                ("MessageBody", RECORD.to_string()),
            ],
        ),
    ];
    for (action, params) in &query_rows {
        let (status, answered) = sqs_query(&router, action, params).await;
        let code = element(&answered, "Code");
        let message = element(&answered, "Message");
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (REFUSAL_STATUS, NON_EXISTENT_QUEUE, QUERY_API_SENTENCE),
            "case {action:?} on the AWS Query surface: expected HTTP {REFUSAL_STATUS} \
             {NON_EXISTENT_QUEUE} reading {QUERY_API_SENTENCE:?}, got {status} with {answered}"
        );
        for value in [SOURCE_QUEUE, source_url.as_str(), account.as_str()] {
            assert!(
                !message.contains(value),
                "case {action:?} on the AWS Query surface: the sentence must interpolate \
                 nothing of the request, got {message:?} naming {value:?}"
            );
        }
    }
}

/// Every call this suite makes on a queue that is not there, each with the rest of a request that
/// is otherwise well formed — so the only thing its refusal can be about is the queue. The first
/// two are the pair a client proves a teardown with (the name resolved, then the url sent to);
/// the others are the same rule on operations the cleanup leg does not itself call.
fn requests_naming(name: &str, url: &str) -> Vec<(String, Value)> {
    vec![
        ("GetQueueUrl", json!({ "QueueName": name })),
        (
            "SendMessage",
            json!({ "QueueUrl": url, "MessageBody": RECORD }),
        ),
        ("ReceiveMessage", json!({ "QueueUrl": url })),
        (
            "GetQueueAttributes",
            json!({ "QueueUrl": url, "AttributeNames": ["All"] }),
        ),
    ]
    .into_iter()
    .map(|(operation, request)| (operation.to_string(), request))
    .collect()
}

/// Assert one JSON answer is the absence refusal, whole: the status, the class, the legacy report
/// beside it, AWS's own sentence, and that the sentence interpolated none of `probed`.
fn assert_json_absence(
    form: &str,
    operation: &str,
    probed: &[&str],
    answered: (u16, http::HeaderMap, Value),
) {
    let (status, headers, body) = answered;
    let report = headers
        .get(QUERY_ERROR_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string);
    let message = message_of(&body);
    assert_eq!(
        (
            status,
            class_of(&body).as_str(),
            message.as_str(),
            report.clone()
        ),
        (
            REFUSAL_STATUS,
            QUEUE_DOES_NOT_EXIST,
            AWS_SENTENCE,
            Some(NON_EXISTENT_QUEUE_REPORT.to_string())
        ),
        "case {form:?} / {operation}: expected HTTP {REFUSAL_STATUS} {QUEUE_DOES_NOT_EXIST} \
         reading {AWS_SENTENCE:?} reported as {NON_EXISTENT_QUEUE_REPORT:?}, got {status} with \
         {body} and {QUERY_ERROR_HEADER} {report:?}"
    );
    for value in probed {
        assert!(
            !message.contains(value),
            "case {form:?} / {operation}: the sentence must interpolate nothing of the request, \
             got {message:?} naming {value:?}"
        );
    }
}

/// The class a JSON refusal names: the local name of its `__type` discriminator.
fn class_of(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit(['#', '/'])
        .next()
        .unwrap_or("<no __type>")
        .to_string()
}

/// The sentence a JSON refusal carries, under either spelling of the member.
fn message_of(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
        .to_string()
}

/// The url a queue called `name` would have beside the queue at `url`: the same endpoint and
/// account path with the other name, which is how a client addresses a queue it believes in.
fn sibling_url(url: &str, name: &str) -> String {
    let (prefix, _) = url
        .rsplit_once('/')
        .unwrap_or_else(|| panic!("a queue url has a final segment, got {url:?}"));
    format!("{prefix}/{name}")
}

/// One Amazon SQS call over its `awsJson1_0` wire, as its status, its headers and the document it
/// answered.
async fn sqs_json(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, http::HeaderMap, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("authorization", authorization("sqs", REGION))
        .header("content-type", JSON_CONTENT_TYPE)
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .body(Bytes::from(body.to_string()))
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

/// The same call, which must be served, answering the document it reported.
async fn ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, _, parsed) = sqs_json(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One Amazon SQS call on the AWS Query form wire, as its status and the XML text it answered.
async fn sqs_query(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(&str, String)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={QUERY_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("authorization", authorization("sqs", REGION))
        .header("content-type", FORM_CONTENT_TYPE)
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One EventBridge Pipes call over its `restJson1` wire, on the URI the operation's `@http`
/// binding declares, as its status and the document it answered.
async fn pipes(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body
        .map(|body| Bytes::from(body.to_string()))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("pipes.{REGION}.amazonaws.com"))
        .header("authorization", authorization("pipes", REGION))
        .header("content-type", REST_CONTENT_TYPE)
        .body(payload)
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// One IAM call: IAM speaks `awsQuery`, so its parameters travel as a form body and its answer
/// comes back as XML.
async fn iam(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("iam.{REGION}.amazonaws.com"))
        .header("authorization", authorization("iam", REGION))
        .header("content-type", FORM_CONTENT_TYPE)
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// A queue called `name`, with the source's own visibility timeout when `source`; answered with
/// its url and the ARN a pipe names it by.
async fn given_queue(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    source: bool,
) -> (String, String) {
    let mut request = json!({ "QueueName": name });
    if source {
        request["Attributes"] = json!({ "VisibilityTimeout": SOURCE_VISIBILITY });
    }
    let created = ok(router, case, "CreateQueue", &request).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueUrl in {created}"))
        .to_string();
    let attributes = ok(
        router,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueArn in {attributes}"))
        .to_string();
    (url, arn)
}

/// A role trusting EventBridge Pipes and allowing exactly the calls a queue-to-queue pipe's
/// stages make — the source's receive, delete and attribute reads, and the target's send, each on
/// the queue the stage makes it on. Answered with the ARN IAM persisted.
async fn given_role(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    source_arn: &str,
    target_arn: &str,
) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "pipes.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let (status, created) = iam(
        router,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateRole answered {status} {created}"
    );
    let arn = element(&created, "Arn");
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": ["sqs:ReceiveMessage", "sqs:DeleteMessage", "sqs:GetQueueAttributes"],
                "Resource": source_arn,
            },
            {
                "Effect": "Allow",
                "Action": "sqs:SendMessage",
                "Resource": target_arn,
            },
        ],
    })
    .to_string();
    let (status, written) = iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "move-records"),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRolePolicy answered {status} {written}"
    );
    arn
}

/// The pipe from `source_arn` to `target_arn`, with no target parameters at all, waited on until
/// it reports `CurrentState` RUNNING — which is when it polls.
async fn given_running_pipe(
    router: &MultiServiceRouter,
    case: &str,
    source_arn: &str,
    target_arn: &str,
    role_arn: &str,
) {
    let (status, created) = pipes(
        router,
        "POST",
        &format!("/v1/pipes/{PIPE}"),
        Some(json!({
            "Source": source_arn,
            "Target": target_arn,
            "RoleArn": role_arn,
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreatePipe answered {status} {created}"
    );
    wait_for_pipe_state(router, case, "RUNNING").await;
}

/// The state the pipe reports now, and the status `DescribePipe` answered with.
async fn described_pipe(router: &MultiServiceRouter) -> (u16, String) {
    let (status, described) = pipes(router, "GET", &format!("/v1/pipes/{PIPE}"), None).await;
    let state = described["CurrentState"].as_str().unwrap_or_default();
    (status, state.to_string())
}

/// Look again until the pipe reports `wanted`, or [`BUDGET`] lapses.
async fn wait_for_pipe_state(router: &MultiServiceRouter, case: &str, wanted: &str) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, state) = described_pipe(router).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribePipe answered {status} while waiting for {wanted}"
        );
        if state == wanted {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the pipe to reach CurrentState {wanted:?}, it reports \
             {state:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Stop the pipe, wait for it to report itself STOPPED, delete it, and wait until EventBridge
/// Pipes holds no such pipe — the teardown order a client tears this topology down in.
async fn stop_and_delete_pipe(router: &MultiServiceRouter, case: &str) {
    let (status, stopped) = pipes(router, "POST", &format!("/v1/pipes/{PIPE}/stop"), None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: StopPipe answered {status} {stopped}"
    );
    wait_for_pipe_state(router, case, "STOPPED").await;
    let (status, deleted) = pipes(router, "DELETE", &format!("/v1/pipes/{PIPE}"), None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DeletePipe answered {status} {deleted}"
    );
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, state) = described_pipe(router).await;
        if status == 404 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the deleted pipe to be gone, DescribePipe answers {status} \
             reporting {state:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Produce one record to the queue at `url`.
async fn send(router: &MultiServiceRouter, case: &str, url: &str, body: &str) {
    ok(
        router,
        case,
        "SendMessage",
        &json!({ "QueueUrl": url, "MessageBody": body }),
    )
    .await;
}

/// Look again at the queue at `url` until one message has arrived, answering its body.
async fn wait_for_delivery(router: &MultiServiceRouter, case: &str, url: &str) -> String {
    let deadline = Instant::now() + BUDGET;
    loop {
        let received = ok(
            router,
            case,
            "ReceiveMessage",
            &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10, "WaitTimeSeconds": 1 }),
        )
        .await;
        if let Some(body) = received["Messages"]
            .as_array()
            .and_then(|messages| messages.first())
            .and_then(|message| message["Body"].as_str())
        {
            return body.to_string();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the pipe to deliver its record to the target queue, the \
             queue is still empty"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Look again until Amazon SQS no longer resolves the queue called `name`, which is what a client
/// proves a delete propagated with.
///
/// The condition is the ABSENCE class itself and not merely a refused call, so a request refused
/// for some other reason keeps the wait going instead of ending it as if the queue were gone.
async fn wait_until_absent(router: &MultiServiceRouter, case: &str, name: &str) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, _, body) = sqs_json(router, "GetQueueUrl", &json!({ "QueueName": name })).await;
        if (status, class_of(&body).as_str()) == (REFUSAL_STATUS, QUEUE_DOES_NOT_EXIST) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the deleted queue {name:?} to stop resolving, GetQueueUrl \
             answers {status} with {body}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}
