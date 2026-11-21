// SPDX-License-Identifier: CC0-1.0

//! What a RUNNING pipe whose `Target` is a STEP FUNCTIONS STATE MACHINE does, end to end
//! over the wire with pipes, Amazon SQS and Step Functions mounted on ONE engine.
//!
//! The RULE asserted over the tables below: how many calls a pipe's target stage makes for
//! one polled batch is decided by the target service's own API and never by the pipe — a
//! target whose API takes a payload that can be the batch receives ONE call carrying every
//! event of it. A state machine is started with one `input` ("the string that contains the
//! JSON input data for the execution") and `PipeTargetStateMachineParameters` carries no
//! batch member, so a pipe hands its state-machine target ONE execution per polled batch,
//! whose input is the JSON array of the events the target stage was handed — which is what
//! makes the source's `BatchSize` both the count of executions a backlog comes to and the
//! count of elements one execution's input holds — and the records it polled leave the
//! source queue once that execution has taken them.
//!
//! WHICH Step Functions call starts it is `InvocationType`'s: `FIRE_AND_FORGET` starts the
//! execution asynchronously (`StartExecution`) and `REQUEST_RESPONSE`, the value the member
//! documents as its default, synchronously (`StartSyncExecution`) — a call Step Functions
//! itself does not serve for a `STANDARD` workflow, so a pipe configured that way against a
//! standard state machine delivers nothing and leaves its records where they were.
//!
//! Nothing in this suite starts an execution: the only data-plane calls a row makes are the
//! `SendMessage`s that put the records on the source queue. Every execution read back, and
//! every message read off a RESULT queue, got there because the pipe started the workflow, so
//! a pipe that reports `RUNNING` and delivers nothing leaves both empty and the row fails.
//!
//! The last table asserts a further RULE about WHAT one of those records is: a pipe polls its
//! source for as long as it is RUNNING — so a record waiting before the pipe existed and a
//! record published while it was already polling are both delivered — and the envelope the
//! target is handed identifies the record the way the SOURCE API does, its `messageId` being
//! the `MessageId` that queue's `SendMessage` answered the producer and its `eventSourceARN`
//! the source queue's own ARN.
//!
//! The rows differ along what the rules generalise over: how many records are waiting, the
//! `BatchSize` they are polled under (one, a divisor of the count, the whole count and the
//! member's absence), whether the target carries an `InputTemplate`, whether the pipe's
//! `FilterCriteria` drop one of the records, which `InvocationType` the target parameters
//! name (both values and the member's absence), which workflow type the target is, which of
//! the two start calls the pipe's execution role may make, and when each record is published
//! relative to the pipe reaching `RUNNING` (before it exists, in one wave after, and in
//! several).
//!
//! The member names and the invocation types are the model's own
//! (`smithy_query(query="shape", shape="PipeTargetParameters,PipeTargetStateMachineParameters")`).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_pipes_pipes::state::execution_role::SERVICE_PRINCIPAL;
use service_com_amazonaws_pipes_pipes::state::pipe_state::RUNNING;
use smithy_server_runtime::MultiServiceRouter;

use crate::pipe_data_plane::{
    collect_at_least, collect_during, counted, emulator, given_queue, markers, rpc, send, sqs,
    wait_for_counted, LOOK_AGAIN, QUIET, VISIBLE_ATTRIBUTE,
};
use crate::pipe_execution_role::{wait_for_state, IN_FLIGHT_ATTRIBUTE};
use crate::pipe_lifecycle::{call, Reply, REGION};
use crate::pipe_metrics::{emulator as emulator_with_iam, given_role_stating};

/// The role every pipe of the first two tables names. Nothing there turns on its policies:
/// no IAM store is mounted in that emulator, so no stage is refused on the strength of one.
/// The third table mounts IAM and states the role's policies itself.
const ROLE: &str = "arn:aws:iam::000000000000:role/pipes-state-machine-target";

/// How long work the pipe owes — settling into `RUNNING`, polling a batch and starting the
/// execution — is given before a row fails. Far above the data plane's poll interval and the
/// span a pipe takes to settle, so a row fails only when the work did not happen at all and a
/// loaded host does not make a slow pass look like a missing one.
const BUDGET: Duration = Duration::from_secs(60);

/// The workflow types a state machine below is created as, as Step Functions' own
/// `StateMachineType` spells them.
const STANDARD: &str = "STANDARD";
/// See [`STANDARD`].
const EXPRESS: &str = "EXPRESS";

/// The `eventSource` an Amazon SQS record envelope reports.
const SQS_EVENT_SOURCE: &str = "aws:sqs";

/// The attributes that together say how many records a queue still HOLDS: the ones waiting on
/// it plus the ones a consumer has taken and not deleted.
const HELD_ATTRIBUTES: [&str; 2] = [VISIBLE_ATTRIBUTE, IN_FLIGHT_ATTRIBUTE];

/// A workflow that hands its input straight back, so what an execution reports as its output
/// is the input the pipe started it with. It is the smallest definition that runs at all.
const ECHO_DEFINITION: &str = r#"{"StartAt":"Echo","States":{"Echo":{"Type":"Pass","End":true}}}"#;

/// The execution statuses that are no longer going to change, as Step Functions' own
/// `ExecutionStatus` spells them.
const SUCCEEDED: &str = "SUCCEEDED";

/// One Step Functions call (`awsJson1_0`, `AWSStepFunctions.<Operation>`).
async fn sfn(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    rpc(router, "states", "AWSStepFunctions", operation, body).await
}

/// A state machine called `name` of the workflow type `sm_type` running `definition`;
/// answers the ARN a pipe names it by.
async fn given_state_machine(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    sm_type: &str,
    definition: &str,
) -> String {
    let (status, created) = sfn(
        router,
        "CreateStateMachine",
        &json!({
            "name": name,
            "definition": definition,
            "roleArn": ROLE,
            "type": sm_type,
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

/// A workflow that reports the FIRST element of its input's `body` on the queue at `url`,
/// through the Step Functions service integration that sends a message
/// (`arn:aws:states:::sqs:sendMessage`).
///
/// It is how a row whose execution no read reports — a synchronous run, and any run of an
/// EXPRESS workflow, which `ListExecutions` and `DescribeExecution` are "not supported by" —
/// is observed: one message on that queue is one execution, carrying the body of the record
/// that execution was started with.
fn report_definition(url: &str) -> String {
    json!({
        "StartAt": "Report",
        "States": {
            "Report": {
                "Type": "Task",
                "Resource": "arn:aws:states:::sqs:sendMessage",
                "Parameters": { "QueueUrl": url, "MessageBody.$": "$[0].body" },
                "End": true,
            }
        }
    })
    .to_string()
}

/// Every execution of the state machine `machine_arn`, newest first, as `ListExecutions`
/// reports them.
async fn executions(router: &MultiServiceRouter, case: &str, machine_arn: &str) -> Vec<Value> {
    let (status, listed) = sfn(
        router,
        "ListExecutions",
        &json!({ "stateMachineArn": machine_arn }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListExecutions answered {status} {listed}"
    );
    listed["executions"].as_array().cloned().unwrap_or_default()
}

/// The executions of `machine_arn`, waited for until there are at least `want` of them or
/// `BUDGET` has elapsed.
async fn executions_at_least(
    router: &MultiServiceRouter,
    case: &str,
    machine_arn: &str,
    want: usize,
) -> Vec<Value> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let listed = executions(router, case, machine_arn).await;
        if listed.len() >= want || Instant::now() >= deadline {
            return listed;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One execution as `DescribeExecution` reports it, read again until it rests in a status it
/// will not leave.
async fn described_execution(
    router: &MultiServiceRouter,
    case: &str,
    execution_arn: &str,
) -> Value {
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
        if described["status"].as_str() != Some("RUNNING") || Instant::now() >= deadline {
            return described;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// `CreatePipe`, answered as the lifecycle harness reads a reply.
async fn create_pipe(router: &MultiServiceRouter, name: &str, body: Value) -> Reply {
    call(
        router,
        "POST",
        &format!("/v1/pipes/{name}"),
        Some(body),
        REGION,
    )
    .await
}

/// The size of each batch a backlog of `records` already waiting is polled in, under a
/// `BatchSize` of `batch_size`: a poll takes as many as are waiting up to that ceiling, so
/// the batches are full until the last one.
fn batch_sizes(records: usize, batch_size: usize) -> Vec<usize> {
    let mut sizes = Vec::new();
    let mut left = records;
    while left > 0 {
        let taken = left.min(batch_size);
        sizes.push(taken);
        left -= taken;
    }
    sizes
}

/// One row of the first table: the records waiting on the source, the `BatchSize` they are
/// polled under, and how the target is configured.
struct Row {
    /// Also the pipe's, the queue's and the state machine's name, so every row is
    /// independent.
    case: &'static str,
    records: usize,
    batch_size: Option<i64>,
    /// `TargetParameters.InputTemplate`, when the row configures one.
    template: Option<&'static str>,
    /// `SourceParameters.FilterCriteria`'s one pattern, when the row carries one, and how
    /// many of the records it admits.
    filter: Option<&'static str>,
    admitted: usize,
}

/// The `CreatePipe` body of one row of the first table: an Amazon SQS source, a state-machine
/// target started asynchronously, and the row's own batching, template and filter.
fn pipe_body(row: &Row, source_arn: &str, target_arn: &str) -> Value {
    let mut queue_parameters = serde_json::Map::new();
    if let Some(batch_size) = row.batch_size {
        queue_parameters.insert("BatchSize".to_string(), json!(batch_size));
    }
    let mut source_parameters = json!({ "SqsQueueParameters": Value::Object(queue_parameters) });
    if let Some(pattern) = row.filter {
        source_parameters["FilterCriteria"] = json!({ "Filters": [{ "Pattern": pattern }] });
    }
    let mut target_parameters = json!({
        "StepFunctionStateMachineParameters": { "InvocationType": "FIRE_AND_FORGET" },
    });
    if let Some(template) = row.template {
        target_parameters["InputTemplate"] = json!(template);
    }
    json!({
        "Source": source_arn,
        "Target": target_arn,
        "RoleArn": ROLE,
        "DesiredState": "RUNNING",
        "SourceParameters": source_parameters,
        "TargetParameters": target_parameters,
    })
}

/// RULE: a pipe whose target is a Step Functions state machine starts ONE execution per
/// polled batch, whose input is the array of the events the target stage was handed — so the
/// source's `BatchSize` decides how many executions a backlog comes to and how many elements
/// one input holds, each element being the record envelope (or what the target's
/// `InputTemplate` rendered from it) — and the records it took leave the source once the
/// execution has them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_state_machine_target_is_started_once_per_polled_batch() {
    let rows = [
        Row {
            case: "one-record-batch-size-one",
            records: 1,
            batch_size: Some(1),
            template: None,
            filter: None,
            admitted: 1,
        },
        Row {
            case: "one-record-default-batch-size",
            records: 1,
            batch_size: None,
            template: None,
            filter: None,
            admitted: 1,
        },
        Row {
            case: "three-records-one-execution",
            records: 3,
            batch_size: Some(5),
            template: None,
            filter: None,
            admitted: 3,
        },
        Row {
            case: "batch-size-one-starts-per-record",
            records: 3,
            batch_size: Some(1),
            template: None,
            filter: None,
            admitted: 3,
        },
        Row {
            case: "four-records-batch-of-two",
            records: 4,
            batch_size: Some(2),
            template: None,
            filter: None,
            admitted: 4,
        },
        Row {
            case: "an-input-template-shapes-each-event",
            records: 2,
            batch_size: Some(10),
            template: Some(r#"{"order": "<$.body>"}"#),
            filter: None,
            admitted: 2,
        },
        Row {
            case: "a-filter-admits-one-of-two-records",
            records: 2,
            batch_size: Some(10),
            template: None,
            // The body of the record the row admits, as an event pattern matches a body
            // that is not a JSON document: by its text.
            filter: Some(r#"{"body": ["a-filter-admits-one-of-two-records-0"]}"#),
            admitted: 1,
        },
    ];
    let router = emulator();
    for row in &rows {
        let case = row.case;
        let (source_url, source_arn) = given_queue(&router, &format!("{case}-source")).await;
        let target_arn =
            given_state_machine(&router, case, case, STANDARD, ECHO_DEFINITION).await;

        let sent = markers(case, row.records);
        send(&router, &source_url, &sent).await;

        let created = create_pipe(&router, case, pipe_body(row, &source_arn, &target_arn)).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreatePipe answered {} {}",
            created.status, created.body
        );

        // The batches the backlog is polled in, and therefore the executions it comes to:
        // one per batch, whatever each batch holds.
        let batches = batch_sizes(
            row.admitted,
            row.batch_size.unwrap_or(10).try_into().unwrap_or(10),
        );
        let started = executions_at_least(&router, case, &target_arn, batches.len()).await;
        assert_eq!(
            started.len(),
            batches.len(),
            "case {case:?}: expected one execution per polled batch ({} of them, of sizes \
             {batches:?}), got {} ({started:?})",
            batches.len(),
            started.len()
        );
        // Nothing MORE is started: a batch whose records the pipe did not consume would be
        // polled and handed over again, which this watch would see.
        tokio::time::sleep(QUIET).await;
        let again = executions(&router, case, &target_arn).await;
        assert_eq!(
            again.len(),
            batches.len(),
            "case {case:?}: expected each polled batch started exactly once, {} execution(s) \
             stand after a further watch: {again:?}",
            again.len()
        );

        // What each execution was started with: ONE array, holding the events of its own
        // batch — and, because the definition hands its input straight back, what it
        // answered as its output.
        let mut events: Vec<Value> = Vec::new();
        for execution in &started {
            let arn = execution["executionArn"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: no executionArn in {execution}"));
            let described = described_execution(&router, case, arn).await;
            assert_eq!(
                described["status"].as_str(),
                Some(SUCCEEDED),
                "case {case:?}: expected the execution to succeed, got {described}"
            );
            let input: Value = serde_json::from_str(
                described["input"].as_str().unwrap_or_default(),
            )
            .unwrap_or_else(|error| {
                panic!("case {case:?}: the execution input is not JSON ({error}): {described}")
            });
            let held = input.as_array().cloned().unwrap_or_else(|| {
                panic!("case {case:?}: expected the input to be an array, got {input}")
            });
            assert!(
                !held.is_empty() && held.len() <= batches[0],
                "case {case:?}: an execution carried {} events, outside the 1..={} one batch \
                 of this pipe holds: {input}",
                held.len(),
                batches[0]
            );
            let output: Value =
                serde_json::from_str(described["output"].as_str().unwrap_or_default())
                    .unwrap_or_else(|error| {
                        panic!(
                            "case {case:?}: the execution output is not JSON ({error}): \
                             {described}"
                        )
                    });
            assert_eq!(
                output, input,
                "case {case:?}: this definition hands its input back, so the output must equal \
                 the input, got {output}"
            );
            events.extend(held);
        }
        assert_eq!(
            events.len(),
            row.admitted,
            "case {case:?}: expected the executions to carry every admitted record exactly \
             once ({} of them), got {} ({events:?})",
            row.admitted,
            events.len()
        );

        // Each event, as the pipe handed it over: the record envelope itself, or what the
        // target's input template rendered from it.
        let mut bodies: BTreeMap<String, usize> = BTreeMap::new();
        for event in &events {
            let body = match row.template {
                None => {
                    assert_eq!(
                        event["eventSource"].as_str(),
                        Some(SQS_EVENT_SOURCE),
                        "case {case:?}: expected the Amazon SQS record envelope, got {event}"
                    );
                    assert_eq!(
                        event["eventSourceARN"].as_str(),
                        Some(source_arn.as_str()),
                        "case {case:?}: expected the source ARN on the envelope, got {event}"
                    );
                    assert!(
                        event["messageId"].as_str().is_some_and(|id| !id.is_empty()),
                        "case {case:?}: expected the record's message id, got {event}"
                    );
                    event["body"].as_str().unwrap_or_default().to_string()
                }
                Some(_) => {
                    assert!(
                        event.get("eventSourceARN").is_none(),
                        "case {case:?}: a rendered event carries nothing of the envelope, got \
                         {event}"
                    );
                    event["order"].as_str().unwrap_or_default().to_string()
                }
            };
            *bodies.entry(body).or_default() += 1;
        }
        let expected: BTreeMap<String, usize> = sent
            .iter()
            .take(row.admitted)
            .map(|marker| (marker.clone(), 1))
            .collect();
        assert_eq!(
            bodies, expected,
            "case {case:?}: expected the admitted records' own bodies once each, got {bodies:?}"
        );

        // Consumed: a batch an execution took is deleted from the source, and a record the
        // filter dropped is consumed too, so the queue ends up holding nothing.
        let held = wait_for_counted(&router, &source_url, &HELD_ATTRIBUTES, 0, BUDGET).await;
        assert_eq!(
            held, 0,
            "case {case:?}: expected the source queue drained once the executions took its \
             records, it still holds {held}"
        );
    }
}

/// One row of the invocation-type table: the workflow type the target is, the
/// `InvocationType` the target parameters name, and whether Step Functions serves the start
/// that invocation type makes for that workflow type.
struct Start {
    /// Also the pipe's, the queues' and the state machine's name.
    case: &'static str,
    sm_type: &'static str,
    /// `TargetParameters.StepFunctionStateMachineParameters.InvocationType`, or `None` for a
    /// pipe that configures no invocation type at all.
    invocation_type: Option<&'static str>,
    /// Whether the execution is started, so the workflow reports the record it was handed.
    started: bool,
}

/// RULE: which Step Functions call a state-machine target is started with is
/// `InvocationType`'s — `FIRE_AND_FORGET` the asynchronous `StartExecution`,
/// `REQUEST_RESPONSE` (the default the member documents, and so the absence of the member)
/// the synchronous `StartSyncExecution`, which Step Functions does not serve for a `STANDARD`
/// workflow — so a pipe whose configuration names a start its target's workflow type does
/// not support delivers nothing and leaves its record on the source.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_invocation_type_decides_which_start_the_target_is_handed_the_batch_by() {
    let rows = [
        Start {
            case: "standard-fire-and-forget",
            sm_type: STANDARD,
            invocation_type: Some("FIRE_AND_FORGET"),
            started: true,
        },
        Start {
            case: "express-fire-and-forget",
            sm_type: EXPRESS,
            invocation_type: Some("FIRE_AND_FORGET"),
            started: true,
        },
        Start {
            case: "express-request-response",
            sm_type: EXPRESS,
            invocation_type: Some("REQUEST_RESPONSE"),
            started: true,
        },
        Start {
            case: "express-no-invocation-type",
            sm_type: EXPRESS,
            invocation_type: None,
            started: true,
        },
        Start {
            case: "standard-request-response",
            sm_type: STANDARD,
            invocation_type: Some("REQUEST_RESPONSE"),
            started: false,
        },
        Start {
            case: "standard-no-invocation-type",
            sm_type: STANDARD,
            invocation_type: None,
            started: false,
        },
    ];
    let router = emulator();
    for row in &rows {
        let case = row.case;
        let (source_url, source_arn) = given_queue(&router, &format!("{case}-source")).await;
        let (result_url, _) = given_queue(&router, &format!("{case}-result")).await;
        let target_arn = given_state_machine(
            &router,
            case,
            case,
            row.sm_type,
            &report_definition(&result_url),
        )
        .await;

        let mut parameters = serde_json::Map::new();
        if let Some(invocation_type) = row.invocation_type {
            parameters.insert("InvocationType".to_string(), json!(invocation_type));
        }
        let created = create_pipe(
            &router,
            case,
            json!({
                "Source": source_arn,
                "Target": target_arn,
                "RoleArn": ROLE,
                "DesiredState": "RUNNING",
                "SourceParameters": { "SqsQueueParameters": { "BatchSize": 1 } },
                "TargetParameters": {
                    "StepFunctionStateMachineParameters": Value::Object(parameters),
                },
            }),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreatePipe answered {} {}",
            created.status, created.body
        );
        let state = wait_for_state(&router, case, RUNNING, BUDGET).await;
        assert_eq!(
            state, RUNNING,
            "case {case:?}: expected the pipe running before anything is sent, got {state:?}"
        );

        let sent = markers(case, 1);
        send(&router, &source_url, &sent).await;

        if row.started {
            let reported = collect_at_least(&router, &result_url, 1, BUDGET).await;
            assert_eq!(
                reported,
                sent,
                "case {case:?}: expected the workflow to report the record it was started \
                 with, got {reported:?}"
            );
            let held = wait_for_counted(&router, &source_url, &HELD_ATTRIBUTES, 0, BUDGET).await;
            assert_eq!(
                held, 0,
                "case {case:?}: expected the source queue drained once the execution took its \
                 record, it still holds {held}"
            );
        } else {
            let reported = collect_during(&router, &result_url, QUIET).await;
            assert!(
                reported.is_empty(),
                "case {case:?}: expected no execution for a start this workflow type does not \
                 support, the workflow reported {reported:?}"
            );
            let held = counted(&router, &source_url, &HELD_ATTRIBUTES).await;
            assert_eq!(
                held, 1,
                "case {case:?}: expected the record left on the source when nothing took it, \
                 the queue holds {held}"
            );
        }
    }
}

/// One row of the authorization table: the action the pipe's execution role is granted on
/// the target, and whether the start the pipe's invocation type makes is one that role may
/// make.
struct Granted {
    /// Also the pipe's, the queue's and the state machine's name.
    case: &'static str,
    /// The action granted on the target state machine, or `None` for a role granted nothing
    /// on it at all.
    action: Option<&'static str>,
    started: bool,
}

/// RULE: the start of a state-machine target is made AS THE PIPE'S EXECUTION ROLE and
/// authorized under the API call the configured invocation type makes — `states:StartExecution`
/// for `FIRE_AND_FORGET` — so a role that may not make THAT call is an execution that never
/// starts, and the record stays on its source to be handed over again once the grant is in
/// force.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_asynchronous_start_is_authorized_under_states_start_execution() {
    let rows = [
        Granted {
            case: "granted-start-execution",
            action: Some("states:StartExecution"),
            started: true,
        },
        Granted {
            case: "granted-nothing-on-the-target",
            action: None,
            started: false,
        },
        Granted {
            // The OTHER start call: a grant of the synchronous one says nothing about the
            // asynchronous start this pipe's invocation type makes.
            case: "granted-only-the-synchronous-start",
            action: Some("states:StartSyncExecution"),
            started: false,
        },
    ];
    let router = emulator_with_iam();
    for row in &rows {
        let case = row.case;
        let (source_url, source_arn) = given_queue(&router, &format!("{case}-source")).await;
        let target_arn =
            given_state_machine(&router, case, case, STANDARD, ECHO_DEFINITION).await;
        let mut statements = vec![json!({
            "Effect": "Allow",
            "Action": [
                "sqs:ReceiveMessage",
                "sqs:DeleteMessage",
                "sqs:GetQueueAttributes",
            ],
            "Resource": source_arn,
        })];
        if let Some(action) = row.action {
            statements.push(json!({
                "Effect": "Allow",
                "Action": action,
                "Resource": target_arn,
            }));
        }
        let role = given_role_stating(
            &router,
            &format!("{case}-role"),
            SERVICE_PRINCIPAL,
            Value::Array(statements),
        )
        .await;

        let created = create_pipe(
            &router,
            case,
            json!({
                "Source": source_arn,
                "Target": target_arn,
                "RoleArn": role,
                "DesiredState": "RUNNING",
                "SourceParameters": { "SqsQueueParameters": { "BatchSize": 1 } },
                "TargetParameters": {
                    "StepFunctionStateMachineParameters": {
                        "InvocationType": "FIRE_AND_FORGET",
                    },
                },
            }),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreatePipe answered {} {}",
            created.status, created.body
        );
        let state = wait_for_state(&router, case, RUNNING, BUDGET).await;
        assert_eq!(
            state, RUNNING,
            "case {case:?}: expected the pipe running before anything is sent, got {state:?}"
        );

        send(&router, &source_url, &markers(case, 1)).await;

        if row.started {
            let started = executions_at_least(&router, case, &target_arn, 1).await;
            assert_eq!(
                started.len(),
                1,
                "case {case:?}: expected one execution for the one record, got {started:?}"
            );
            let held = wait_for_counted(&router, &source_url, &HELD_ATTRIBUTES, 0, BUDGET).await;
            assert_eq!(
                held, 0,
                "case {case:?}: expected the source queue drained once the execution took its \
                 record, it still holds {held}"
            );
        } else {
            tokio::time::sleep(QUIET).await;
            let started = executions(&router, case, &target_arn).await;
            assert!(
                started.is_empty(),
                "case {case:?}: expected no execution for a role that may not start one, got \
                 {started:?}"
            );
            let held = counted(&router, &source_url, &HELD_ATTRIBUTES).await;
            assert_eq!(
                held, 1,
                "case {case:?}: expected the record left on the source when the start was \
                 withheld, the queue holds {held}"
            );
        }
    }
}

/// One row of the record-identity table: when the records are published relative to the
/// pipe's own lifecycle, and the `BatchSize` the pipe polls them under.
struct Published {
    /// Also the pipe's, the queue's and the state machine's name.
    case: &'static str,
    /// How many records are already waiting on the source when the pipe is created.
    waiting: usize,
    /// How many records are sent once the pipe reports `RUNNING`, one entry per wave — so a
    /// row says both how many arrive after the pipe was already polling and in how many
    /// separate publications they do.
    waves: &'static [usize],
    batch_size: i64,
}

impl Published {
    /// Every record this row publishes, waiting and later ones together.
    fn records(&self) -> usize {
        self.waiting + self.waves.iter().sum::<usize>()
    }
}

/// Send each of `bodies` to the queue at `url`, answering the `MessageId` Amazon SQS assigned
/// each of them: "an attribute containing the MessageId of the message sent to the queue"
/// (`SendMessage`'s `SendMessageResult$MessageId`) — the identity its producer is handed, and
/// so the identity a delivery of that record has to be recognisable by.
async fn send_identified(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    bodies: &[String],
) -> BTreeMap<String, String> {
    let mut identified: BTreeMap<String, String> = BTreeMap::new();
    for body in bodies {
        let (status, answered) = sqs(
            router,
            "SendMessage",
            &json!({ "QueueUrl": url, "MessageBody": body }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: SendMessage answered {status} {answered}"
        );
        let id = answered["MessageId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no MessageId in {answered}"))
            .to_string();
        assert!(
            identified.insert(id.clone(), body.clone()).is_none(),
            "case {case:?}: SendMessage answered the message id {id:?} for two records"
        );
    }
    identified
}

/// Every record the executions of `machine_arn` were started with, as `(messageId, body)`
/// pairs in the order the executions report them — one pass over every execution there is.
///
/// Each execution's input is read as the pipe handed it over: ONE JSON array of record
/// envelopes, each carrying the source it was polled from.
async fn delivered_records(
    router: &MultiServiceRouter,
    case: &str,
    machine_arn: &str,
    source_arn: &str,
    batch_size: usize,
) -> Vec<(String, String)> {
    let mut records: Vec<(String, String)> = Vec::new();
    for execution in executions(router, case, machine_arn).await {
        let arn = execution["executionArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no executionArn in {execution}"));
        let described = described_execution(router, case, arn).await;
        assert_eq!(
            described["status"].as_str(),
            Some(SUCCEEDED),
            "case {case:?}: expected the execution to succeed, got {described}"
        );
        let input: Value = serde_json::from_str(described["input"].as_str().unwrap_or_default())
            .unwrap_or_else(|error| {
                panic!("case {case:?}: the execution input is not JSON ({error}): {described}")
            });
        // The batch of source records itself: an ARRAY, whatever one batch holds — never one
        // record on its own and never a re-wrapped envelope holding it.
        let held = input.as_array().cloned().unwrap_or_else(|| {
            panic!("case {case:?}: expected the input to be an array of records, got {input}")
        });
        assert!(
            !held.is_empty() && held.len() <= batch_size,
            "case {case:?}: an execution carried {} records, outside the 1..={batch_size} one \
             batch of this pipe holds: {input}",
            held.len()
        );
        for event in held {
            assert_eq!(
                event["eventSource"].as_str(),
                Some(SQS_EVENT_SOURCE),
                "case {case:?}: expected the Amazon SQS record envelope, got {event}"
            );
            assert_eq!(
                event["eventSourceARN"].as_str(),
                Some(source_arn),
                "case {case:?}: expected the source queue's own ARN on the envelope, got {event}"
            );
            let id = event["messageId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: no messageId on the record {event}"))
                .to_string();
            let body = event["body"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: no body on the record {event}"))
                .to_string();
            records.push((id, body));
        }
    }
    records
}

/// The records the executions of `machine_arn` carry, read again until `want` of them have
/// been delivered or `BUDGET` has elapsed.
async fn delivered_at_least(
    router: &MultiServiceRouter,
    case: &str,
    machine_arn: &str,
    source_arn: &str,
    batch_size: usize,
    want: usize,
) -> Vec<(String, String)> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let records = delivered_records(router, case, machine_arn, source_arn, batch_size).await;
        if records.len() >= want || Instant::now() >= deadline {
            return records;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// RULE: a pipe polls its source for as long as it is RUNNING, so every record its source
/// holds is delivered whether it was waiting before the pipe existed or published while the
/// pipe was already polling; and the envelope the target is handed identifies that record the
/// way the SOURCE API does — for an Amazon SQS source the `messageId` is the `MessageId` that
/// queue's `SendMessage` answered its producer and the `eventSourceARN` is the source queue's
/// own ARN — so a producer can recognise each record it published, exactly once, in what the
/// target ran.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_delivered_record_carries_the_message_id_its_producer_was_answered() {
    let rows = [
        Published {
            case: "waiting-before-the-pipe-exists",
            waiting: 2,
            waves: &[],
            batch_size: 1,
        },
        Published {
            // The record arrives once the pipe is already RUNNING: its first poll after that
            // picks the record up, because the record is durable on the queue.
            case: "published-while-the-pipe-is-running",
            waiting: 0,
            waves: &[1],
            batch_size: 1,
        },
        Published {
            case: "published-in-two-waves-while-running",
            waiting: 0,
            waves: &[1, 2],
            batch_size: 1,
        },
        Published {
            case: "waiting-records-and-a-later-wave",
            waiting: 2,
            waves: &[2],
            batch_size: 2,
        },
        Published {
            // One record under a batch size far above it: the input is still the array of the
            // records of that batch, with the one record in it.
            case: "one-record-under-a-wide-batch",
            waiting: 0,
            waves: &[1],
            batch_size: 10,
        },
    ];
    let router = emulator();
    for row in &rows {
        let case = row.case;
        let (source_url, source_arn) = given_queue(&router, &format!("{case}-source")).await;
        let target_arn = given_state_machine(&router, case, case, STANDARD, ECHO_DEFINITION).await;

        let bodies = markers(case, row.records());
        let mut published =
            send_identified(&router, case, &source_url, &bodies[..row.waiting]).await;

        let created = create_pipe(
            &router,
            case,
            json!({
                "Source": source_arn,
                "Target": target_arn,
                "RoleArn": ROLE,
                "DesiredState": "RUNNING",
                "SourceParameters": { "SqsQueueParameters": { "BatchSize": row.batch_size } },
                "TargetParameters": {
                    "StepFunctionStateMachineParameters": {
                        "InvocationType": "FIRE_AND_FORGET",
                    },
                },
            }),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreatePipe answered {} {}",
            created.status, created.body
        );
        let state = wait_for_state(&router, case, RUNNING, BUDGET).await;
        assert_eq!(
            state, RUNNING,
            "case {case:?}: expected the pipe running before the later records are sent, got \
             {state:?}"
        );

        let mut next = row.waiting;
        for wave in row.waves {
            let wave_bodies = &bodies[next..next + wave];
            next += wave;
            published.extend(send_identified(&router, case, &source_url, wave_bodies).await);
        }

        let batch_size = usize::try_from(row.batch_size).unwrap_or(1);
        let delivered = delivered_at_least(
            &router,
            case,
            &target_arn,
            &source_arn,
            batch_size,
            published.len(),
        )
        .await;
        let mut seen: BTreeMap<String, String> = BTreeMap::new();
        for (id, body) in &delivered {
            let before = seen.insert(id.clone(), body.clone());
            assert!(
                before.is_none(),
                "case {case:?}: the record {id:?} was delivered more than once ({delivered:?})"
            );
        }
        assert_eq!(
            seen, published,
            "case {case:?}: expected each published record delivered once under the message id \
             SendMessage answered for it, got {seen:?}"
        );

        // Nothing MORE is delivered: a record the executions took is deleted from its source,
        // so no later poll hands it over again.
        tokio::time::sleep(QUIET).await;
        let again = delivered_records(&router, case, &target_arn, &source_arn, batch_size).await;
        assert_eq!(
            again.len(),
            delivered.len(),
            "case {case:?}: expected every record delivered exactly once, {} record(s) stand \
             after a further watch: {again:?}",
            again.len()
        );
        let held = wait_for_counted(&router, &source_url, &HELD_ATTRIBUTES, 0, BUDGET).await;
        assert_eq!(
            held, 0,
            "case {case:?}: expected the source queue drained once the executions took its \
             records, it still holds {held}"
        );
    }
}
