// SPDX-License-Identifier: CC0-1.0

//! WHERE the record of a pipe whose execution role withholds one of its stages' calls ends up.
//!
//! RULE: every call a pipe's stage makes is decided against the policies in force on the pipe's
//! execution role BEFORE it is made, so a call the role does not admit never HAPPENS — it is not
//! a call that is made and then undone. For an Amazon SQS source that is the difference a client
//! reads off the queue: a pipe whose role withholds the source's `sqs:ReceiveMessage` takes
//! nothing out of that queue at all, so the record is still WAITING on it
//! ("ApproximateNumberOfMessages — the approximate number of messages available for retrieval")
//! with nothing in flight ("ApproximateNumberOfMessagesNotVisible — the approximate number of
//! messages that are in flight"), and another consumer's own `ReceiveMessage` is still handed it;
//! while a pipe whose role admits that receive and withholds the TARGET's call DOES take the
//! record — it is in flight with the pipe — and still delivers nothing; and a pipe whose role
//! admits every call its stages make delivers the record and leaves the source holding nothing.
//!
//! Three crates have to agree for the rule to hold — EventBridge Pipes runs the pipe and asks the
//! question, IAM holds the role whose documents answer it, and Amazon SQS holds the source whose
//! own counts are the observation — so the suite lives here rather than in any one of them.
//!
//! What is new here is the DISTINCTION, which the suites that assert a withheld call moves nothing
//! do not draw: `core/router/tests/all/cloudformation_deployed_pipe_delivery.rs` and
//! `services/service-com-amazonaws-pipes-pipes/tests/all/pipe_execution_role.rs` count the records
//! a withheld pipe left on its source as the SUM of the two attributes, which a record never taken
//! and a record taken and stuck both satisfy. The sum is what a client reading a backlog sees; the
//! two attributes apart are what tells it whether the queue can still hand that record to anyone
//! else.
//!
//! The rows differ along what the rule generalises over: WHICH of the calls its stages make the
//! role withholds (the source's receive, the target's send, none of them) and HOW it withholds it
//! (the action left out of the statement that covers the source, the action granted on another
//! queue, an explicit `Deny` over a wildcard allow).
//!
//! Every observation is one a client makes: `GetQueueAttributes` and `ReceiveMessage` on the
//! source, `ReceiveMessage` on the target, and `DescribePipe` for the state the pipe rests in.
//! Each wait is on a CONDITION with a generous deadline, and nothing below asserts on elapsed
//! time.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{form_encode, opt_element};

/// The Region every call below is signed for, which is what each handler reads its Region from.
const REGION: &str = "us-east-1";

/// The `CurrentState` a pipe whose `DesiredState` is the default comes to rest in, which is when
/// it polls.
const RUNNING: &str = "RUNNING";

/// How long work the pipe does on its own is given before a case fails. Far above the data
/// plane's own poll interval and the span a pipe takes to reach `RUNNING`, so a case fails only
/// when the pipe did not do the work at all.
const BUDGET: Duration = Duration::from_secs(30);

/// How long an assertion that NOTHING happens watches for — no delivery at the target, and the
/// source's counts unchanged. A pipe that was going to act would have: it is many times the
/// poller's own interval and several times its retry backoff.
const QUIET: Duration = Duration::from_secs(3);

/// How often a queue or a pipe is looked at again while waiting.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The `VisibilityTimeout` every source queue is created with, in seconds: long enough that a
/// record the pipe TOOK is still in flight with it when the case looks, so a record that is
/// available again is one no consumer is holding rather than one whose checkout lapsed.
const SOURCE_VISIBILITY: &str = "60";

/// `TargetParameters.InputTemplate`, so a delivered message carries the record's own body and a
/// delivery can be attributed to the record that caused it: `<$.body>` selects the `body` of the
/// record envelope the target stage is handed.
const BODY_TEMPLATE: &str = "<$.body>";

/// The calls an Amazon SQS source's stage makes ON THE SOURCE. Source: AWS's documented required
/// permissions for an Amazon SQS pipe source.
const RECEIVE: &str = "sqs:ReceiveMessage";
const DELETE: &str = "sqs:DeleteMessage";
const GET_ATTRIBUTES: &str = "sqs:GetQueueAttributes";

/// The call an Amazon SQS target's stage makes ON THE TARGET.
const SEND: &str = "sqs:SendMessage";

/// "ApproximateNumberOfMessages — the approximate number of messages available for retrieval
/// from the queue" (`GetQueueAttributes$AttributeNames`): the records the queue can still hand
/// to a consumer, which is what a record no consumer has taken is one of.
const AVAILABLE: &str = "ApproximateNumberOfMessages";

/// "ApproximateNumberOfMessagesNotVisible — the approximate number of messages that are in
/// flight": the records a consumer has taken and not deleted, which is what a record the pipe
/// took and could not deliver is one of.
const IN_FLIGHT: &str = "ApproximateNumberOfMessagesNotVisible";

/// The one body every case sends to its source.
const RECORD_BODY: &str = r#"{"marker": "the-one-record"}"#;

/// Where the record a case sent ends up, which is what its row asserts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The record is never TAKEN: it stays available on the source for the whole quiet window,
    /// another consumer's own receive is handed it, and the target receives nothing.
    StaysAvailable,
    /// The record IS taken — it is in flight with the pipe — and the target still receives
    /// nothing.
    TakenAndUndelivered,
    /// The record reaches the target, and the source is left holding nothing.
    Delivered,
}

/// The queues one case runs over, by the ARN each statement names them by.
struct Queues {
    source_arn: String,
    target_arn: String,
    /// A queue the pipe never touches, for the row whose grant names another queue.
    other_arn: String,
}

/// One row: how the pipe's execution role is written, and where that leaves the record.
struct Row {
    case: &'static str,
    /// The statements of the role's one inline policy, built from the case's own queue ARNs —
    /// what the rule turns on is the (action, RESOURCE) pair each call is made on.
    statements: fn(&Queues) -> Value,
    outcome: Outcome,
}

/// An `Allow` of `actions` on `resource`.
fn allow(actions: Value, resource: &str) -> Value {
    json!({ "Effect": "Allow", "Action": actions, "Resource": resource })
}

/// A `Deny` of `actions` on `resource`.
fn deny(actions: Value, resource: &str) -> Value {
    json!({ "Effect": "Deny", "Action": actions, "Resource": resource })
}

/// RULE: see the module docs — a call the execution role withholds is never made, so the source's
/// own counts say whether the pipe took the record at all.
#[tokio::test]
async fn a_call_the_execution_role_withholds_is_never_made() {
    let rows = [
        Row {
            // The statement that covers the source leaves the RECEIVE out, so the pipe may look
            // at its source and delete from it but never take a record out of it.
            case: "receive-left-out-of-the-source-statement",
            statements: |queues| {
                json!([
                    allow(json!([DELETE, GET_ATTRIBUTES]), &queues.source_arn),
                    allow(json!(SEND), &queues.target_arn),
                ])
            },
            outcome: Outcome::StaysAvailable,
        },
        Row {
            // The receive IS granted — on another queue. A grant that names a resource the call
            // is not made on admits nothing.
            case: "receive-granted-on-another-queue",
            statements: |queues| {
                json!([
                    allow(json!(RECEIVE), &queues.other_arn),
                    allow(json!([DELETE, GET_ATTRIBUTES]), &queues.source_arn),
                    allow(json!(SEND), &queues.target_arn),
                ])
            },
            outcome: Outcome::StaysAvailable,
        },
        Row {
            // Every queue call allowed by a wildcard, with the receive on THIS source denied: a
            // `Deny` decides whatever an `Allow` says.
            case: "receive-denied-over-a-wildcard-allow",
            statements: |queues| {
                json!([
                    allow(json!("sqs:*"), "*"),
                    deny(json!(RECEIVE), &queues.source_arn),
                ])
            },
            outcome: Outcome::StaysAvailable,
        },
        Row {
            // The other side of the rule: the source stage's calls are all admitted and the
            // TARGET's is not, so the record is taken and then goes nowhere.
            case: "target-send-withheld-while-the-receive-is-granted",
            statements: |queues| {
                json!([allow(
                    json!([RECEIVE, DELETE, GET_ATTRIBUTES]),
                    &queues.source_arn
                )])
            },
            outcome: Outcome::TakenAndUndelivered,
        },
        Row {
            // The control: every call the stages make, each on the queue it is made on.
            case: "every-call-the-stages-make-admitted",
            statements: |queues| {
                json!([
                    allow(
                        json!([RECEIVE, DELETE, GET_ATTRIBUTES]),
                        &queues.source_arn
                    ),
                    allow(json!(SEND), &queues.target_arn),
                ])
            },
            outcome: Outcome::Delivered,
        },
    ];

    for row in &rows {
        run_row(row).await;
    }
}

/// Keep the table honest: every row is driven by this one body.
async fn run_row(row: &Row) {
    let case = row.case;
    let router = emulator();
    let (source_url, source_arn) = given_queue(&router, case, &format!("{case}-src"), true).await;
    let (target_url, target_arn) = given_queue(&router, case, &format!("{case}-tgt"), false).await;
    let (_other_url, other_arn) = given_queue(&router, case, &format!("{case}-other"), false).await;
    let queues = Queues {
        source_arn,
        target_arn,
        other_arn,
    };
    let role_arn = given_role(
        &router,
        case,
        &format!("{case}-role"),
        (row.statements)(&queues),
    )
    .await;

    // A role that withholds only SOME of the calls its source stage makes leaves the pipe to come
    // up, and the withheld call is then one of its executions failing: so every row's pipe rests
    // in the state its `DesiredState` asked for, whatever its role admits.
    given_pipe(&router, case, &queues, &role_arn).await;
    send(&router, case, &source_url, RECORD_BODY).await;

    match row.outcome {
        Outcome::StaysAvailable => {
            // The invariant has to HOLD, not just be reached: the counts are read again and again
            // over the whole window, and a single reading that says the pipe took the record
            // fails the row.
            let readings = counts_while_quiet(&router, case, &source_url).await;
            for (available, in_flight) in &readings {
                assert_eq!(
                    (*available, *in_flight),
                    (1, 0),
                    "case {case:?}: expected the record to stay available on the source with \
                     nothing in flight ({AVAILABLE} 1, {IN_FLIGHT} 0), one reading said \
                     ({available}, {in_flight}); every reading: {readings:?}"
                );
            }
            // What the counts mean for another consumer: the queue can still hand that record
            // over. The receive takes no checkout of its own, so nothing this asserts is hidden
            // from the assertions after it.
            let offered = receive_now(&router, case, &source_url).await;
            assert!(
                offered.contains(&RECORD_BODY.to_string()),
                "case {case:?}: expected the source to hand the record {RECORD_BODY:?} to \
                 another consumer, it offered {offered:?}"
            );
            assert_no_delivery(&router, case, &target_url).await;
        }
        Outcome::TakenAndUndelivered => {
            let in_flight = wait_for_count(&router, case, &source_url, IN_FLIGHT, 1).await;
            assert_eq!(
                in_flight, 1,
                "case {case:?}: expected the pipe to have taken the record out of the source \
                 ({IN_FLIGHT} 1), the source reports {in_flight}"
            );
            assert_no_delivery(&router, case, &target_url).await;
            // Taken is not consumed: the record is still the queue's, so it comes back when the
            // checkout lapses.
            let available = count(&router, case, &source_url, AVAILABLE).await;
            assert_eq!(
                in_flight + available,
                1,
                "case {case:?}: expected the source to still hold the record it handed over, it \
                 reports {AVAILABLE} {available} and {IN_FLIGHT} {in_flight}"
            );
        }
        Outcome::Delivered => {
            let delivered = collect_at_least(&router, case, &target_url, 1).await;
            assert_eq!(
                delivered,
                BTreeSet::from([RECORD_BODY.to_string()]),
                "case {case:?}: expected the target to receive the record's own body \
                 {RECORD_BODY:?}, it received {delivered:?}"
            );
            let held = wait_for_held(&router, case, &source_url, 0).await;
            assert_eq!(
                held, 0,
                "case {case:?}: expected the delivered record to be deleted from the source, it \
                 still holds {held}"
            );
        }
    }
}

/// Nothing reaches the target over the whole quiet window.
async fn assert_no_delivery(router: &MultiServiceRouter, case: &str, target_url: &str) {
    let until = Instant::now() + QUIET;
    let mut seen: BTreeSet<String> = BTreeSet::new();
    while Instant::now() < until {
        seen.extend(drain(router, case, target_url).await);
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    assert!(
        seen.is_empty(),
        "case {case:?}: expected the target to receive nothing, it received {seen:?}"
    );
}

/// Pipes, Amazon SQS and IAM on ONE engine, exactly as `core/router/src/lib.rs` mounts them.
/// Request authentication is off, as in every other suite here; the calls the pipe's own stages
/// make are signed either way (the credential scope is what routes them).
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_pipes_pipes::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in [`REGION`], which is
/// what routes the request. The signature is never checked: this router runs with request
/// authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// One Amazon SQS call over its `awsJson1.0` wire.
async fn sqs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .header("authorization", authorization("sqs"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice::<Value>(&payload).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One EventBridge Pipes call over its `restJson1` wire, on the URI the operation's `@http`
/// binding declares (`/v1/pipes/{Name}`).
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
        .header("content-type", "application/json")
        .header("authorization", authorization("pipes"))
        .body(payload)
        .expect("a well-formed request");
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
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("iam"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
}

/// A queue called `name`, with the source's own visibility timeout when `source`; answered with
/// its url and the ARN a pipe and a policy statement name it by.
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
    let (status, created) = sqs(router, "CreateQueue", &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateQueue {name:?} answered {status} {created}"
    );
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueUrl in {created}"))
        .to_string();
    let (status, attributes) = sqs(
        router,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetQueueAttributes answered {status} {attributes}"
    );
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueArn in {attributes}"))
        .to_string();
    (url, arn)
}

/// A role trusting EventBridge Pipes — the service that acts as a pipe's `RoleArn` — and carrying
/// `statements` as its one inline policy. Answered with the ARN IAM persisted.
async fn given_role(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    statements: Value,
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
    let arn =
        opt_element(&created, "Arn").unwrap_or_else(|| panic!("case {case:?}: no Arn in {created}"));
    let document = json!({ "Version": "2012-10-17", "Statement": statements }).to_string();
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

/// A pipe named after the case, from the case's source to its target under `role_arn`, polling one
/// record per batch and delivering each record's own body; waited on until it reports
/// `CurrentState` [`RUNNING`], which is when it polls.
async fn given_pipe(
    router: &MultiServiceRouter,
    case: &str,
    queues: &Queues,
    role_arn: &str,
) {
    let body = json!({
        "Source": queues.source_arn,
        "Target": queues.target_arn,
        "RoleArn": role_arn,
        "SourceParameters": { "SqsQueueParameters": { "BatchSize": 1 } },
        "TargetParameters": { "InputTemplate": BODY_TEMPLATE },
    });
    let (status, created) = pipes(router, "POST", &format!("/v1/pipes/{case}"), Some(body)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreatePipe answered {status} {created}"
    );
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, described) = pipes(router, "GET", &format!("/v1/pipes/{case}"), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribePipe answered {status} {described}"
        );
        let state = described["CurrentState"].as_str().unwrap_or_default();
        if state == RUNNING {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the pipe to reach CurrentState {RUNNING}, it reports \
             {state:?} ({described})"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// `SendMessage` of `body` to the queue at `url`.
async fn send(router: &MultiServiceRouter, case: &str, url: &str, body: &str) {
    let (status, sent) = sqs(
        router,
        "SendMessage",
        &json!({ "QueueUrl": url, "MessageBody": body }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SendMessage answered {status} {sent}"
    );
}

/// One `ReceiveMessage` of up to ten messages at `url`, as bodies, left on the queue:
/// `VisibilityTimeout: 0` takes no checkout, so a later look sees them again and nothing this
/// suite reads is hidden from the assertion after it.
async fn receive_now(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<String> {
    let (status, answered) = sqs(
        router,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10, "VisibilityTimeout": 0 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ReceiveMessage answered {status} {answered}"
    );
    answered["Messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|message| message["Body"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// One `ReceiveMessage` at `url`, each message DELETED, so a later look does not count a delivery
/// twice.
async fn drain(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<String> {
    let (status, received) = sqs(
        router,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ReceiveMessage answered {status} {received}"
    );
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut bodies = Vec::new();
    for message in &messages {
        bodies.push(message["Body"].as_str().unwrap_or_default().to_string());
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            let (status, deleted) = sqs(
                router,
                "DeleteMessage",
                &json!({ "QueueUrl": url, "ReceiptHandle": handle }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: DeleteMessage answered {status} {deleted}"
            );
        }
    }
    bodies
}

/// Every body delivered to `url`, collected until there are at least `want` of them or [`BUDGET`]
/// lapses.
async fn collect_at_least(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    want: usize,
) -> BTreeSet<String> {
    let deadline = Instant::now() + BUDGET;
    let mut collected: BTreeSet<String> = BTreeSet::new();
    loop {
        collected.extend(drain(router, case, url).await);
        if collected.len() >= want || Instant::now() >= deadline {
            return collected;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One attribute of the queue at `url`, as the count `GetQueueAttributes` reports for it.
async fn count(router: &MultiServiceRouter, case: &str, url: &str, attribute: &str) -> i64 {
    let (status, answered) = sqs(
        router,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": [attribute] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetQueueAttributes answered {status} {answered}"
    );
    answered["Attributes"][attribute]
        .as_str()
        .unwrap_or_default()
        .parse()
        .unwrap_or(-1)
}

/// The two counts of the queue at `url`: what is available on it and what is in flight with a
/// consumer.
async fn counts(router: &MultiServiceRouter, case: &str, url: &str) -> (i64, i64) {
    (
        count(router, case, url, AVAILABLE).await,
        count(router, case, url, IN_FLIGHT).await,
    )
}

/// Every reading of those two counts over the whole of [`QUIET`], for the row whose rule is that
/// they do not change.
async fn counts_while_quiet(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
) -> Vec<(i64, i64)> {
    let until = Instant::now() + QUIET;
    let mut readings = Vec::new();
    while Instant::now() < until {
        readings.push(counts(router, case, url).await);
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    readings
}

/// Wait until the queue at `url` reports `want` for `attribute`, answering what it reported last.
async fn wait_for_count(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    attribute: &str,
    want: i64,
) -> i64 {
    let deadline = Instant::now() + BUDGET;
    loop {
        let counted = count(router, case, url, attribute).await;
        if counted == want || Instant::now() >= deadline {
            return counted;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Wait until the queue at `url` HOLDS `want` records — the ones waiting on it plus the ones in
/// flight with a consumer — answering what it held last.
async fn wait_for_held(router: &MultiServiceRouter, case: &str, url: &str, want: i64) -> i64 {
    let deadline = Instant::now() + BUDGET;
    loop {
        let (available, in_flight) = counts(router, case, url).await;
        let held = available + in_flight;
        if held == want || Instant::now() >= deadline {
            return held;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}
