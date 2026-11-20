// SPDX-License-Identifier: CC0-1.0

//! WHAT the execution a pipe starts for its Step Functions target was handed: the SOURCE records
//! themselves, each one as Amazon SQS reported it to the pipe's poller.
//!
//! Three crates have to agree for the rule to hold — EventBridge Pipes runs the pipe with no client
//! action, Amazon SQS holds the source whose messages it polls, Step Functions holds the state
//! machine it starts and the reads that report the execution — so the suite lives here rather than
//! in any one of them. Every call goes through `MultiServiceRouter::dispatch`, the one path a
//! socket-served request takes.
//!
//! RULE: a RUNNING pipe whose `Source` names an Amazon SQS queue and whose `Target` names a Step
//! Functions state machine polls that queue itself and starts ONE execution per polled batch whose
//! `input` is the JSON array of the SQS record ENVELOPES of that batch — not a re-wrapped payload —
//! so each element carries exactly the members the source API reports for a message (`messageId`,
//! `receiptHandle`, `body`, `md5OfBody`, `attributes`, `messageAttributes`) beside the three the
//! poller adds to say where the record came from (`eventSource`, `eventSourceARN`, `awsRegion`),
//! with the identifier and the digest Amazon SQS ITSELF answered for that message, the body the
//! producer sent byte for byte, `eventSourceARN` naming the source queue and `awsRegion` the Region
//! it was polled in; and that execution is one `ListExecutions` reports for the target machine and
//! `DescribeExecution` describes.
//!
//! `services/service-com-amazonaws-pipes-pipes/tests/all/pipe_state_machine_target.rs` asks how many
//! executions a backlog comes to and which start each invocation type makes; what is new here is
//! WHICH RECORD each execution was handed — the envelope's whole member set, and its identifier and
//! body digest held against what Amazon SQS answered for that very message — over bodies of every
//! form, for records sent to a pipe that is already running as well as for a backlog, read back
//! through a bounded listing page.
//!
//! What makes each read an answer about the PIPE: nothing here starts an execution and nothing here
//! receives from the source queue. The only data-plane call a row makes is the `SendMessage` that
//! puts a record on the source, so every execution read back exists because the pipe polled that
//! record and started the workflow with it.
//!
//! The rows differ along what the rule generalises over: how many records are handed over (one,
//! three, four), the `BatchSize` they are polled under (one, a divisor of the count, a ceiling above
//! it, and the member's absence), what a body IS (a JSON document, a nested document, a JSON array,
//! a JSON string, digits, a negative decimal, text spelling no document, text outside ASCII, text
//! holding newlines and spaces, a single character, a kilobyte of it), whether the records were
//! already waiting when the pipe was created or were sent to it once it reported RUNNING, and
//! whether the listing is asked for a bounded page or for everything.
//!
//! The envelope's member names are the Amazon SQS `Message` shape's own, lowercase-initialled
//! (`smithy_query(query="shape", shape="Message")` on the Amazon SQS model), plus the three the
//! poller adds; `md5OfMessageAttributes` is the one member of that shape a record carries only when
//! the message has message attributes, and no row here sends any. The source and target parameter
//! members are the pipes model's own (`smithy_query(query="shape",
//! shape="PipeSourceSqsQueueParameters,PipeTargetStateMachineParameters")`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call below is signed for, which is what each handler reads its Region from, and
/// so what the record envelopes report as the Region the records were polled in.
const REGION: &str = "us-east-1";

/// The role every pipe below names. Nothing here turns on its policies: the engine carries no IAM
/// store, which says nothing about what the role may do.
const ROLE: &str = "arn:aws:iam::000000000000:role/pipes-sqs-state-machine-target";

/// The role each state machine below is created with. Step Functions records it; the echo
/// definition calls no service, so nothing reads it.
const MACHINE_ROLE: &str = "arn:aws:iam::000000000000:role/states-echo";

/// The `CurrentState` a pipe that is polling its source rests in.
const RUNNING: &str = "RUNNING";

/// The workflow type every target below is created as, as Step Functions' own `StateMachineType`
/// spells it: the type whose executions `ListExecutions` and `DescribeExecution` report.
const STANDARD: &str = "STANDARD";

/// `PipeTargetStateMachineParameters.InvocationType`'s asynchronous value — the one that starts the
/// execution with `StartExecution`, the call a `STANDARD` workflow serves.
const FIRE_AND_FORGET: &str = "FIRE_AND_FORGET";

/// The execution status a run that is over and succeeded rests in, as Step Functions' own
/// `ExecutionStatus` spells it.
const SUCCEEDED: &str = "SUCCEEDED";

/// The status an execution that has not finished reports, which is what is waited out.
const EXECUTION_RUNNING: &str = "RUNNING";

/// The `eventSource` an Amazon SQS record envelope reports.
const SQS_EVENT_SOURCE: &str = "aws:sqs";

/// A workflow that hands its input straight back, so what an execution reports as its output is the
/// input the pipe started it with. It is the smallest definition that runs at all.
const ECHO_DEFINITION: &str = r#"{"StartAt":"Echo","States":{"Echo":{"Type":"Pass","End":true}}}"#;

/// How long work the pipe owes — settling into RUNNING, polling a batch and starting the execution
/// — is given before a row fails. Far above the data plane's own poll interval and the span a pipe
/// takes to settle, so a row fails only when the work did not happen at all and neither a loaded
/// host nor a busy test run makes a slow pass look like a missing one.
const BUDGET: Duration = Duration::from_secs(120);

/// How long a count that must not GROW is watched for, once everything the pipe owes has arrived. A
/// further start that was going to be made would have: it is many times the poller's own interval.
const QUIET: Duration = Duration::from_secs(3);

/// How often a pipe, a queue or a state machine is looked at again while waiting for a condition.
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// The `VisibilityTimeout` each source queue is created with: a record the pipe took is hidden from
/// every other consumer for this long, so a record handed over twice is one the pipe did not finish
/// with rather than one a slow host let reappear. It is far above the window between a start and the
/// delete that follows it, so no case turns on how fast the host runs.
const SOURCE_VISIBILITY: &str = "300";

/// Exactly the members an Amazon SQS record envelope carries for a message sent with no message
/// attributes: the `Message` shape's own, lowercase-initialled, plus the three the poller adds.
const ENVELOPE_MEMBERS: [&str; 9] = [
    "attributes",
    "awsRegion",
    "body",
    "eventSource",
    "eventSourceARN",
    "md5OfBody",
    "messageAttributes",
    "messageId",
    "receiptHandle",
];

/// Pipes, Amazon SQS and Step Functions on ONE engine, exactly as `core/router/src/lib.rs` mounts
/// them. Request authentication is off, as in every other suite here.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_pipes_pipes::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_sfn_awsstepfunctions::EmulatedService::new(engine));
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

/// One `awsJson` RPC call against the service whose signing name is `signing_name`.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(signing_name))
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

/// One Amazon SQS call.
async fn sqs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    rpc(router, "sqs", "AmazonSQS", operation, body).await
}

/// One Step Functions call.
async fn sfn(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    rpc(router, "states", "AWSStepFunctions", operation, body).await
}

/// One EventBridge Pipes call over its `restJson1` wire, on the URI the operation's `@http` binding
/// declares.
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

/// A queue called `name`; answers its url and the ARN a pipe names it by.
async fn given_queue(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let (status, created) = sqs(
        router,
        "CreateQueue",
        &json!({
            "QueueName": name,
            "Attributes": { "VisibilityTimeout": SOURCE_VISIBILITY },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateQueue answered {status} {created}"
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

/// A `STANDARD` state machine called `name` running the echo definition; answers the ARN a pipe
/// names it by.
async fn given_state_machine(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, created) = sfn(
        router,
        "CreateStateMachine",
        &json!({
            "name": name,
            "definition": ECHO_DEFINITION,
            "roleArn": MACHINE_ROLE,
            "type": STANDARD,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStateMachine answered {status} {created}"
    );
    created["stateMachineArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no stateMachineArn in {created}"))
        .to_string()
}

/// What Amazon SQS itself answered about one sent message: the body it stored and the digest of it —
/// the values the record envelope the pipe hands over must carry for that message.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Sent {
    body: String,
    digest: String,
}

/// Send `body` to the queue at `url`; answers its `MessageId` beside what Amazon SQS reported for
/// it.
async fn send(router: &MultiServiceRouter, case: &str, url: &str, body: &str) -> (String, Sent) {
    let (status, answer) = sqs(
        router,
        "SendMessage",
        &json!({ "QueueUrl": url, "MessageBody": body }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SendMessage answered {status} {answer}"
    );
    let id = answer["MessageId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no MessageId in {answer}"))
        .to_string();
    let digest = answer["MD5OfMessageBody"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no MD5OfMessageBody in {answer}"))
        .to_string();
    (
        id,
        Sent {
            body: body.to_string(),
            digest,
        },
    )
}

/// Wait until the pipe `name` reports it RUNS, which is the state it polls its source in.
async fn wait_until_running(router: &MultiServiceRouter, case: &str, name: &str) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, described) = pipes(router, "GET", &format!("/v1/pipes/{name}"), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribePipe answered {status} {described}"
        );
        if described["CurrentState"].as_str() == Some(RUNNING) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the pipe to reach {RUNNING}, got {described}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Every execution of `machine_arn` as `ListExecutions` reports them — asked for a page of at most
/// `limit`, when the row bounds the page at all.
async fn executions(
    router: &MultiServiceRouter,
    case: &str,
    machine_arn: &str,
    limit: Option<usize>,
) -> Vec<Value> {
    let mut body = json!({ "stateMachineArn": machine_arn });
    if let Some(limit) = limit {
        body["maxResults"] = json!(limit);
    }
    let (status, listed) = sfn(router, "ListExecutions", &body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListExecutions answered {status} {listed}"
    );
    // Every row below starts fewer executions than the page it asks for holds, so the first page is
    // all of them: a `nextToken` would mean this read is no longer the whole answer.
    assert!(
        listed["nextToken"].is_null(),
        "case {case:?}: expected one page to hold every execution, got {listed}"
    );
    listed["executions"].as_array().cloned().unwrap_or_default()
}

/// The executions of `machine_arn`, read again until there are at least `want` of them or the budget
/// lapses.
async fn executions_at_least(
    router: &MultiServiceRouter,
    case: &str,
    machine_arn: &str,
    limit: Option<usize>,
    want: usize,
) -> Vec<Value> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let listed = executions(router, case, machine_arn, limit).await;
        if listed.len() >= want || Instant::now() >= deadline {
            return listed;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The MOST executions `machine_arn` is seen to hold while it is looked at again and again for the
/// whole of [`QUIET`], starting from the `want` already standing: what makes "nothing further was
/// started" an answer rather than a race.
async fn most_executions_during_a_watch(
    router: &MultiServiceRouter,
    case: &str,
    machine_arn: &str,
    limit: Option<usize>,
    want: usize,
) -> usize {
    let deadline = Instant::now() + QUIET;
    let mut most = want;
    while Instant::now() < deadline {
        most = most.max(executions(router, case, machine_arn, limit).await.len());
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    most
}

/// One execution as `DescribeExecution` reports it, read again until it rests in a status it will
/// not leave.
async fn described_execution(router: &MultiServiceRouter, case: &str, execution_arn: &str) -> Value {
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, described) = sfn(
            router,
            "DescribeExecution",
            &json!({ "executionArn": execution_arn }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeExecution answered {status} {described}"
        );
        if described["status"].as_str() != Some(EXECUTION_RUNNING) {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the execution to settle, got {described}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The JSON `text` parses to, or a panic naming the case and which member of the execution it was.
fn parsed(case: &str, what: &str, text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|error| {
        panic!("case {case:?}: the execution {what} is not JSON ({error}): {text:?}")
    })
}

/// One row: the bodies handed to the pipe, the `BatchSize` they are polled under, when they were
/// sent, and how the listing is paged.
struct Row {
    /// Also the pipe's, the queue's and the state machine's name, so every row is independent.
    case: &'static str,
    /// The bodies the row sends, in send order.
    bodies: Vec<String>,
    /// `SourceParameters.SqsQueueParameters.BatchSize`, when the row configures one.
    batch_size: Option<usize>,
    /// Whether the records are sent to a pipe that already reports RUNNING, rather than waiting on
    /// the queue before the pipe exists.
    after_running: bool,
    /// `ListExecutions.maxResults`, when the row bounds the page it asks for.
    list_limit: Option<usize>,
}

/// The sizes of the batches a row's records are polled in, and so the executions they come to: ONE
/// per batch.
///
/// A backlog already waiting when the pipe is created is polled in batches full to the configured
/// ceiling until the last one. A record SENT to a pipe that is already polling is handed over before
/// the next is sent, so such a row comes to one batch per record — which is why every one of those
/// rows either configures a `BatchSize` of one or sends a single record, and why no row needs an
/// assumption about what the absent member defaults to.
fn expected_batches(row: &Row) -> Vec<usize> {
    let records = row.bodies.len();
    if row.after_running {
        return vec![1; records];
    }
    let ceiling = row.batch_size.unwrap_or(records).max(1);
    let mut sizes = Vec::new();
    let mut left = records;
    while left > 0 {
        let taken = left.min(ceiling);
        sizes.push(taken);
        left -= taken;
    }
    sizes
}

/// The `CreatePipe` body of one row: an Amazon SQS source polled under the row's batch size, and a
/// state-machine target started asynchronously.
fn pipe_body(row: &Row, source_arn: &str, target_arn: &str) -> Value {
    let mut queue_parameters = serde_json::Map::new();
    if let Some(batch_size) = row.batch_size {
        queue_parameters.insert("BatchSize".to_string(), json!(batch_size));
    }
    json!({
        "Source": source_arn,
        "Target": target_arn,
        "RoleArn": ROLE,
        "DesiredState": RUNNING,
        "SourceParameters": { "SqsQueueParameters": Value::Object(queue_parameters) },
        "TargetParameters": {
            "StepFunctionStateMachineParameters": { "InvocationType": FIRE_AND_FORGET },
        },
    })
}

/// Drive ONE row and assert the rule over it: the executions the pipe started, the batch each was
/// handed, and the record envelopes they carry held against what Amazon SQS answered for every
/// message sent.
async fn assert_the_records_reach_the_target(row: &Row) {
    let case = row.case;
    let router = emulator();
    let (source_url, source_arn) = given_queue(&router, case, &format!("{case}-source")).await;
    let target_arn = given_state_machine(&router, case, case).await;

    // What Amazon SQS answered for each message, by the identifier it minted: what the record
    // envelope the pipe hands over must agree with.
    let mut sent: BTreeMap<String, Sent> = BTreeMap::new();
    if !row.after_running {
        for body in &row.bodies {
            let (id, reported) = send(&router, case, &source_url, body).await;
            sent.insert(id, reported);
        }
    }

    let (status, created) = pipes(
        &router,
        "POST",
        &format!("/v1/pipes/{case}"),
        Some(pipe_body(row, &source_arn, &target_arn)),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreatePipe must succeed, got {status} {created}"
    );
    wait_until_running(&router, case, case).await;

    if row.after_running {
        for body in &row.bodies {
            let (id, reported) = send(&router, case, &source_url, body).await;
            sent.insert(id, reported);
            // One record per batch for these rows: the next is sent only once the pipe has started
            // the execution for this one, so what each execution was handed is decided.
            let want = sent.len();
            let listed = executions_at_least(&router, case, &target_arn, row.list_limit, want).await;
            assert_eq!(
                listed.len(),
                want,
                "case {case:?}: expected {want} execution(s) after {want} record(s), got {} \
                 ({listed:?})",
                listed.len()
            );
        }
    }
    assert_eq!(
        sent.len(),
        row.bodies.len(),
        "case {case:?}: every send must mint its own message id, got {sent:?}"
    );

    let batches = expected_batches(row);
    let started =
        executions_at_least(&router, case, &target_arn, row.list_limit, batches.len()).await;
    assert_eq!(
        started.len(),
        batches.len(),
        "case {case:?}: expected one execution per polled batch ({} of them, of sizes {batches:?}), \
         got {} ({started:?})",
        batches.len(),
        started.len()
    );
    // Nothing MORE is started: a batch whose records the pipe did not consume would be polled and
    // handed over again, which this watch would see.
    let most =
        most_executions_during_a_watch(&router, case, &target_arn, row.list_limit, started.len())
            .await;
    assert_eq!(
        most,
        batches.len(),
        "case {case:?}: expected each polled batch started exactly once, {most} execution(s) stood \
         during a further watch"
    );

    // What each execution was started with: ONE array of the records of its own batch — and, because
    // the definition hands its input straight back, what it answered as its output.
    let mut records: Vec<Value> = Vec::new();
    let mut carried: Vec<usize> = Vec::new();
    for execution in &started {
        assert_eq!(
            execution["stateMachineArn"].as_str(),
            Some(target_arn.as_str()),
            "case {case:?}: expected the listing to name the target machine, got {execution}"
        );
        let arn = execution["executionArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no executionArn in {execution}"));
        let described = described_execution(&router, case, arn).await;
        assert_eq!(
            described["status"].as_str(),
            Some(SUCCEEDED),
            "case {case:?}: expected the execution to succeed, got {described}"
        );
        let input = parsed(case, "input", described["input"].as_str().unwrap_or_default());
        let held = input.as_array().cloned().unwrap_or_else(|| {
            panic!("case {case:?}: expected the input to be an array of records, got {input}")
        });
        let output = parsed(
            case,
            "output",
            described["output"].as_str().unwrap_or_default(),
        );
        assert_eq!(
            output, input,
            "case {case:?}: this definition hands its input back, so the output must equal the \
             input, got {output}"
        );
        carried.push(held.len());
        records.extend(held);
    }
    carried.sort_unstable();
    let mut wanted_sizes = batches.clone();
    wanted_sizes.sort_unstable();
    assert_eq!(
        carried, wanted_sizes,
        "case {case:?}: expected the executions to carry the records of one polled batch each, got \
         inputs of {carried:?} record(s)"
    );

    // Each record, by the identifier Amazon SQS minted for the message it came from: the whole
    // envelope and nothing else, agreeing with what the source reported for that message.
    let mut delivered: BTreeMap<String, Sent> = BTreeMap::new();
    for record in &records {
        let members: BTreeSet<&str> = record
            .as_object()
            .unwrap_or_else(|| panic!("case {case:?}: expected a record object, got {record}"))
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            members,
            ENVELOPE_MEMBERS.into_iter().collect::<BTreeSet<&str>>(),
            "case {case:?}: expected the Amazon SQS record envelope itself, got {record}"
        );
        assert_eq!(
            record["eventSource"].as_str(),
            Some(SQS_EVENT_SOURCE),
            "case {case:?}: expected the Amazon SQS event source, got {record}"
        );
        assert_eq!(
            record["eventSourceARN"].as_str(),
            Some(source_arn.as_str()),
            "case {case:?}: expected the source queue's ARN on the envelope, got {record}"
        );
        assert_eq!(
            record["awsRegion"].as_str(),
            Some(REGION),
            "case {case:?}: expected the Region the record was polled in, got {record}"
        );
        assert!(
            record["receiptHandle"]
                .as_str()
                .is_some_and(|handle| !handle.is_empty()),
            "case {case:?}: expected the receipt handle the poll handed over, got {record}"
        );
        let id = record["messageId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no messageId in {record}"))
            .to_string();
        let reported = Sent {
            body: record["body"].as_str().unwrap_or_default().to_string(),
            digest: record["md5OfBody"].as_str().unwrap_or_default().to_string(),
        };
        assert!(
            delivered.insert(id.clone(), reported).is_none(),
            "case {case:?}: the record {id} was handed over twice: {records:?}"
        );
    }
    assert_eq!(
        delivered, sent,
        "case {case:?}: expected the executions to carry each sent message once, by the id, body \
         and body digest Amazon SQS answered for it, got {delivered:?}"
    );
}

/// RULE, for records SENT to a pipe that already reports RUNNING — the shape a client drives: the
/// pipe polls them with no client action and starts an execution of its state-machine target whose
/// input is the JSON array of the source record envelopes, each carrying exactly the members the
/// source API reports plus the three the poller adds, with the identifier and body digest Amazon SQS
/// itself answered for that message, the body as sent, the source queue's ARN and the Region it was
/// polled in.
///
/// The rows differ along what the rule generalises over: how many records are sent (one, three), the
/// `BatchSize` they are polled under (one, and the member's absence), what a body IS (a JSON
/// document, text spelling no document, a JSON array, digits, text outside ASCII), and whether the
/// listing is asked for a bounded page or for everything.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_record_sent_to_a_running_pipe_starts_its_state_machine_target() {
    let rows = vec![
        Row {
            case: "sent-after-running-one-json-record",
            bodies: vec![r#"{"marker":"sent-after-running-one-json-record"}"#.to_string()],
            batch_size: Some(1),
            after_running: true,
            list_limit: Some(5),
        },
        Row {
            // No `BatchSize` at all, and a body spelling no document: the envelope carries the bytes
            // the producer sent, whatever they spell.
            case: "sent-after-running-plain-text",
            bodies: vec!["plain text, no document at all".to_string()],
            batch_size: None,
            after_running: true,
            list_limit: None,
        },
        Row {
            // Three records sent one at a time, each its own batch: a JSON array, digits, and text
            // outside ASCII.
            case: "sent-after-running-three-records-one-each",
            bodies: vec![
                "[1,2,3]".to_string(),
                "42".to_string(),
                "αβγ — ünïcode".to_string(),
            ],
            batch_size: Some(1),
            after_running: true,
            list_limit: Some(5),
        },
    ];
    for row in &rows {
        assert_the_records_reach_the_target(row).await;
    }
}

/// RULE, for a BACKLOG already waiting when the pipe is created: the same records reach the same
/// target the same way, in batches full to the configured ceiling — so the `BatchSize` decides how
/// many executions the backlog comes to and how many records one execution's input holds, while each
/// record is still the envelope Amazon SQS reported.
///
/// The rows differ along what the rule generalises over: how many records are waiting (three, four),
/// the `BatchSize` they are polled under (a ceiling above the count, one, a divisor of it), what a
/// body IS (a nested document, a single character, text holding newlines and spaces, a kilobyte of
/// text, a negative decimal, a JSON string), and whether the listing is asked for a bounded page.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_backlog_reaches_its_state_machine_target_one_execution_per_polled_batch() {
    let rows = vec![
        Row {
            // A ceiling above the count: ONE execution carrying every record.
            case: "backlog-of-three-in-one-batch",
            bodies: vec![
                r#"{"nested":{"k":"v"},"n":1}"#.to_string(),
                "a".to_string(),
                "  two spaces and\na newline\t".to_string(),
            ],
            batch_size: Some(10),
            after_running: false,
            list_limit: Some(5),
        },
        Row {
            // The same count polled one at a time: three executions of one record each.
            case: "backlog-of-three-batch-size-one",
            bodies: vec![
                r#"{"marker":"backlog-of-three-batch-size-one-0"}"#.to_string(),
                r#"{"marker":"backlog-of-three-batch-size-one-1"}"#.to_string(),
                r#"{"marker":"backlog-of-three-batch-size-one-2"}"#.to_string(),
            ],
            batch_size: Some(1),
            after_running: false,
            list_limit: None,
        },
        Row {
            // A ceiling that divides the count, and a body of a kilobyte: two executions of two.
            case: "backlog-of-four-in-batches-of-two",
            bodies: vec![
                "x".repeat(1024),
                r#"{"marker":"backlog-of-four-in-batches-of-two-1"}"#.to_string(),
                "-0.5".to_string(),
                r#""a JSON string""#.to_string(),
            ],
            batch_size: Some(2),
            after_running: false,
            list_limit: Some(5),
        },
    ];
    for row in &rows {
        assert_the_records_reach_the_target(row).await;
    }
}
