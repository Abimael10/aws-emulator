// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the two readers whose answer is bounded by the SIZE of what it
//! carries rather than only by a count — one page of an execution's history — and for
//! the document a nested-execution (`.sync`) task reports, driven over this service's
//! own wire protocol (`awsJson1_0`, `POST /` with
//! `X-Amz-Target: AWSStepFunctions.<Operation>`) through the production
//! `MultiServiceRouter`.
//!
//! The rules under test:
//!
//! 1. `maxResults` "is only an upper limit. The actual number of results returned per
//!    call might be fewer than the specified maximum": one page of history is bounded
//!    by the data its events carry as well as by the count, so a history of
//!    large-payload events answers a short page with a `nextToken`, and the pages of
//!    one history still concatenate to exactly that history, in order and with nothing
//!    repeated;
//! 2. the document a `Task` state reports for a service integration is the invoked
//!    operation's response in the integration's own convention: the operation's own
//!    members in PascalCase, its instants in the SDK's own date type (whole epoch
//!    milliseconds), and NOT the wire protocol's type discriminator, which is a tag of
//!    the protocol rather than a member of the response;
//! 3. a run-a-job task whose STARTING call the invoked service refused records the
//!    task's own failure, because nothing was submitted and the refusal is what the
//!    task ended as;
//! 4. the cause a failed job's task reports is the invoked service's report of the job
//!    with the job's own INPUT redacted, while every member that says what the job
//!    ended as is kept.
//!
//! The harness (`Emulator`, `create_machine`, `start_execution`) is the one
//! `state_machine_lifecycle` builds; `tests/all/main.rs` picks this file up, so each is
//! reached through `crate::<suite>`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::{
    EVENT_TASK_FAILED, EVENT_TASK_SUCCEEDED, MAX_HISTORY_PAGE_BYTES,
};

use crate::state_machine_lifecycle::{
    create_machine, describe_execution, start_execution, string, Emulator, REGION, ROLE_ARN,
};

/// How long a test waits for an execution to leave `RUNNING` before giving up.
///
/// A nested execution's task is resolved by polling the child, so the parent's outcome
/// arrives a few poll intervals after the child's: this is well past that and far
/// short of the ceiling on a parked job.
const PATIENCE: Duration = Duration::from_secs(20);

/// The settled record of `execution_arn`, polled until it leaves `RUNNING`.
async fn settled(emulator: &Emulator, case: &str, execution_arn: &str) -> Value {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let described = describe_execution(emulator, execution_arn).await;
        if described.get("status").and_then(Value::as_str) != Some("RUNNING") {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the execution to settle, got {described:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// One page of an execution's history: the events it carries and the token that
/// continues it.
async fn page(emulator: &Emulator, request: &Value) -> (Vec<Value>, Option<String>) {
    let answered = emulator.ok("GetExecutionHistory", request).await;
    let events = answered
        .get("events")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("expected an events list in {answered:?}"))
        .clone();
    let token = answered
        .get("nextToken")
        .and_then(Value::as_str)
        .map(str::to_string);
    (events, token)
}

// ---------------------------------------------------------------------------
// 1. A page of history is bounded by its bytes too
// ---------------------------------------------------------------------------

/// A history of large-payload events answers a page SHORTER than `maxResults`, with a
/// `nextToken`, and the pages of that history concatenate to the whole of it; a history
/// of small events is answered in one page with no token.
///
/// The rows vary the dimension the rule generalises over: how large the data each event
/// carries is, from a payload of a few bytes to one of tens of kilobytes, against the
/// same event count.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_of_history_is_bounded_by_the_data_its_events_carry() {
    /// A machine whose `states` `Pass` states each put their whole input out again, so
    /// every state's pair of events carries the execution's payload.
    fn relay(states: usize) -> String {
        let mut declared = serde_json::Map::new();
        for index in 0..states {
            let mut state = json!({ "Type": "Pass" });
            match index + 1 == states {
                true => state["End"] = json!(true),
                false => state["Next"] = json!(format!("S{}", index + 1)),
            }
            declared.insert(format!("S{index}"), state);
        }
        json!({ "StartAt": "S0", "States": Value::Object(declared) }).to_string()
    }

    // (case, the bytes one event's payload carries, whether one page holds it all).
    // The large rows are stated as fractions of the bound the page carries, so the
    // table follows that bound rather than restating a number; each stays inside what
    // ONE state's document may carry.
    let rows: Vec<(&str, usize, bool)> = vec![
        ("a payload of a few bytes", 8, true),
        ("a payload of a kilobyte", 1_024, true),
        (
            "a payload that fills the page in a handful of events",
            MAX_HISTORY_PAGE_BYTES / 8,
            false,
        ),
        (
            "a payload that fills the page in fewer events still",
            MAX_HISTORY_PAGE_BYTES / 6,
            false,
        ),
    ];
    // Twelve relaying states: 26 events, which is inside the default page size, so the
    // COUNT never ends a page here and only the data can.
    const STATES: usize = 12;
    const EVENTS: usize = STATES * 2 + 2;

    let emulator = Emulator::new();
    let machine = create_machine(&emulator, "byte-bounded", &relay(STATES)).await;
    for (index, (case, payload, one_page)) in rows.iter().enumerate() {
        let input = json!({ "pad": "p".repeat(*payload) }).to_string();
        let execution = start_execution(
            &emulator,
            &machine,
            &format!("payload-{index}"),
            &input,
        )
        .await;
        let described = settled(&emulator, case, &execution).await;
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some("SUCCEEDED"),
            "case {case:?}: expected the relay to succeed, got {described:?}"
        );

        let request = json!({ "executionArn": execution.clone() });
        let (first, token) = page(&emulator, &request).await;
        assert_eq!(
            token.is_none(),
            *one_page,
            "case {case:?}: expected {} page(s) for {} events of {payload} bytes, got \
             {} events and token {token:?}",
            if *one_page { "one" } else { "several" },
            EVENTS,
            first.len()
        );
        if *one_page {
            assert_eq!(
                first.len(),
                EVENTS,
                "case {case:?}: expected the whole history in one page, got {} events",
                first.len()
            );
            continue;
        }
        assert!(
            first.len() < EVENTS,
            "case {case:?}: expected a page shorter than the {EVENTS}-event history, got {}",
            first.len()
        );
        assert!(
            !first.is_empty(),
            "case {case:?}: expected a page to carry at least its first event"
        );

        // The pages walk the rest of the history: every event once, in order.
        let mut walked = first;
        let mut token = token;
        while let Some(cursor) = token {
            let mut request = request.clone();
            request["nextToken"] = json!(cursor);
            let (events, next) = page(&emulator, &request).await;
            assert!(
                !events.is_empty(),
                "case {case:?}: a token names a page that carries events, got {events:?}"
            );
            walked.extend(events);
            token = next;
            assert!(
                walked.len() <= EVENTS,
                "case {case:?}: the pages must not repeat an event, got {} of {EVENTS}",
                walked.len()
            );
        }
        let ids: Vec<i64> = walked
            .iter()
            .filter_map(|event| event.get("id").and_then(Value::as_i64))
            .collect();
        let expected: Vec<i64> = (1..=i64::try_from(EVENTS).expect("the count fits")).collect();
        assert_eq!(
            ids, expected,
            "case {case:?}: expected the pages to concatenate to the whole history, got {ids:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 2-4. What a nested-execution (`.sync`) task reports
// ---------------------------------------------------------------------------

/// A child definition that succeeds with a static result.
const CHILD_SUCCEEDS: &str =
    r#"{"StartAt":"Done","States":{"Done":{"Type":"Pass","Result":{"ok":true},"End":true}}}"#;

/// A child definition that fails with an error and a cause of its own.
const CHILD_FAILS: &str = r#"{"StartAt":"Stop","States":{"Stop":{"Type":"Fail",
    "Error":"Child.Refused","Cause":"the child said so"}}}"#;

/// A parent whose one `Task` state runs the child at `child_arn` through the
/// nested-execution integration `resource`, with `input` as the child's own input.
fn parent_definition(resource: &str, child_arn: &str, input: Value) -> String {
    json!({
        "StartAt": "RunChild",
        "States": {
            "RunChild": {
                "Type": "Task",
                "Resource": resource,
                "Parameters": { "StateMachineArn": child_arn, "Input": input },
                "End": true,
            }
        }
    })
    .to_string()
}

/// The first event of `execution_arn`'s history of the type `event_type`.
async fn event(emulator: &Emulator, execution_arn: &str, event_type: &str) -> Option<Value> {
    let (events, _) = page(
        emulator,
        &json!({ "executionArn": execution_arn, "maxResults": 1000 }),
    )
    .await;
    events
        .into_iter()
        .find(|event| event.get("type").and_then(Value::as_str) == Some(event_type))
}

/// The document a task event's `output` or `cause` carries.
fn document(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|_| panic!("expected a JSON document, got {text:?}"))
}

/// A succeeding child's task reports the child's own `DescribeExecution` document in
/// the integration's convention: PascalCase members, the data details as the modelled
/// structure's one member, instants in the SDK's own date type, and no wire tag.
///
/// The rows are the two revisions of the pattern — `.sync` and `.sync:2` — which differ
/// only in whether the documents the child carries are reported parsed, the dimension
/// the revision generalises over.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_nested_execution_task_reports_the_child_in_the_integrations_convention() {
    // (case, the Resource, whether the revision parses the child's documents)
    let rows: [(&str, &str, bool); 2] = [
        (
            "the first revision of the pattern",
            "arn:aws:states:::states:startExecution.sync",
            false,
        ),
        (
            "the second revision, which parses the documents",
            "arn:aws:states:::states:startExecution.sync:2",
            true,
        ),
    ];

    let emulator = Emulator::new();
    let child = create_machine(&emulator, "sync-shape-child", CHILD_SUCCEEDS).await;
    for (index, (case, resource, parsed)) in rows.into_iter().enumerate() {
        let definition = parent_definition(resource, &child, json!({ "mode": "ok" }));
        let parent = create_machine(&emulator, &format!("sync-shape-{index}"), &definition).await;
        let started = emulator
            .ok(
                "StartExecution",
                &json!({ "stateMachineArn": parent, "name": format!("shape-{index}") }),
            )
            .await;
        let execution = string(&started, "executionArn");
        let parent_started = describe_execution(&emulator, &execution)
            .await
            .get("startDate")
            .and_then(Value::as_f64)
            .expect("the parent reports when it started");

        let described = settled(&emulator, case, &execution).await;
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some("SUCCEEDED"),
            "case {case:?}: expected the parent to succeed, got {described:?}"
        );
        let succeeded = event(&emulator, &execution, EVENT_TASK_SUCCEEDED)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: expected a {EVENT_TASK_SUCCEEDED} event"));
        let reported = document(
            succeeded
                .get("taskSucceededEventDetails")
                .and_then(|details| details.get("output"))
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?}: expected the task's output")),
        );

        for member in [
            "ExecutionArn",
            "StateMachineArn",
            "Name",
            "Status",
            "StartDate",
            "StopDate",
            "Input",
            "InputDetails",
            "Output",
            "OutputDetails",
        ] {
            assert!(
                reported.get(member).is_some(),
                "case {case:?}: expected the member {member:?} of the child's record, got \
                 {reported}"
            );
        }
        assert_eq!(
            reported.get("Status").and_then(Value::as_str),
            Some("SUCCEEDED"),
            "case {case:?}: expected the child's own status, got {reported}"
        );
        // `CloudWatchEventsExecutionDataDetails` has exactly one member, `included`
        // (`smithy_query(query="shape",shape="CloudWatchEventsExecutionDataDetails")`),
        // so that one member in the integration's convention is the whole structure.
        for member in ["InputDetails", "OutputDetails"] {
            assert_eq!(
                reported.get(member),
                Some(&json!({ "Included": true })),
                "case {case:?}: expected {member} to be the modelled structure's one member, \
                 got {reported}"
            );
        }
        // The instants are the SDK's own date type: whole milliseconds since the
        // epoch, so the child cannot read as having started before its parent did.
        let start_date = reported
            .get("StartDate")
            .and_then(Value::as_f64)
            .unwrap_or_else(|| panic!("case {case:?}: expected the child's StartDate"));
        assert!(
            start_date >= parent_started * 1_000.0,
            "case {case:?}: expected the child's StartDate in epoch milliseconds, at or after \
             the parent's start {parent_started}, got {start_date}"
        );
        // The documents the child carries are reported parsed by the second revision
        // of the pattern and as the text the service answered by the first.
        let input = reported
            .get("Input")
            .unwrap_or_else(|| panic!("case {case:?}: expected the child's Input"));
        assert_eq!(
            input.is_object(),
            parsed,
            "case {case:?}: expected the child's Input {} parsed, got {input}",
            if parsed { "" } else { "not" }
        );
    }
}

/// A nested-execution task whose child FAILED fails with the child's own report as its
/// cause, with the child's input redacted from it and every member that says what the
/// child ended as kept.
///
/// The rows are the two revisions of the pattern again: the redaction is the pattern's,
/// not the revision's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_childs_cause_redacts_its_input_and_keeps_what_it_ended_as() {
    let rows: [(&str, &str); 2] = [
        (
            "the first revision of the pattern",
            "arn:aws:states:::states:startExecution.sync",
        ),
        (
            "the second revision of the pattern",
            "arn:aws:states:::states:startExecution.sync:2",
        ),
    ];

    let emulator = Emulator::new();
    let child = create_machine(&emulator, "sync-failure-child", CHILD_FAILS).await;
    for (index, (case, resource)) in rows.into_iter().enumerate() {
        // The child's input carries a marker: it must not travel into the parent's
        // history through the cause.
        let marker = format!("marker-{index}-not-for-the-cause");
        let definition =
            parent_definition(resource, &child, json!({ "secret": marker.clone() }));
        let parent = create_machine(&emulator, &format!("sync-failed-{index}"), &definition).await;
        let started = emulator
            .ok(
                "StartExecution",
                &json!({ "stateMachineArn": parent, "name": format!("failed-{index}") }),
            )
            .await;
        let execution = string(&started, "executionArn");

        let described = settled(&emulator, case, &execution).await;
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some("FAILED"),
            "case {case:?}: expected the parent to fail with its child, got {described:?}"
        );
        let failed = event(&emulator, &execution, EVENT_TASK_FAILED)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: expected a {EVENT_TASK_FAILED} event"));
        let details = failed
            .get("taskFailedEventDetails")
            .unwrap_or_else(|| panic!("case {case:?}: expected the event's own detail"));
        let cause = document(
            details
                .get("cause")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?}: expected the failure's cause")),
        );
        for redacted in ["Input", "InputDetails"] {
            assert_eq!(
                cause.get(redacted),
                None,
                "case {case:?}: the cause must not carry {redacted:?}, got {cause}"
            );
        }
        assert!(
            !cause.to_string().contains(&marker),
            "case {case:?}: the child's input must not travel into the cause, got {cause}"
        );
        for kept in [
            "ExecutionArn",
            "StateMachineArn",
            "Name",
            "Status",
            "Error",
            "Cause",
            "RedriveCount",
            "RedriveStatus",
        ] {
            assert!(
                cause.get(kept).is_some(),
                "case {case:?}: expected the cause to keep {kept:?}, got {cause}"
            );
        }
        assert_eq!(
            cause.get("Status").and_then(Value::as_str),
            Some("FAILED"),
            "case {case:?}: expected the child's own status in the cause, got {cause}"
        );
        assert_eq!(
            cause.get("Error").and_then(Value::as_str),
            Some("Child.Refused"),
            "case {case:?}: expected the child's own error in the cause, got {cause}"
        );
    }
}

/// A nested-execution task whose child is an EXPRESS state machine runs that child to
/// completion: the workflow type has no execution-scoped read for the pattern to poll,
/// so the task ends when the child does, with the child's own record as its result.
///
/// The rows vary what the child ends as — succeeding and failing — which is the
/// dimension the outcome of a run generalises over, over both revisions of the
/// pattern.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_nested_express_child_is_run_to_completion() {
    // (case, the Resource, the child definition, the status the parent must reach)
    let rows: Vec<(&str, &str, &str, &str)> = vec![
        (
            "a succeeding Express child, first revision",
            "arn:aws:states:::states:startExecution.sync",
            CHILD_SUCCEEDS,
            "SUCCEEDED",
        ),
        (
            "a succeeding Express child, second revision",
            "arn:aws:states:::states:startExecution.sync:2",
            CHILD_SUCCEEDS,
            "SUCCEEDED",
        ),
        (
            "a failing Express child",
            "arn:aws:states:::states:startExecution.sync:2",
            CHILD_FAILS,
            "FAILED",
        ),
    ];

    let emulator = Emulator::new();
    for (index, (case, resource, child_definition, expected)) in rows.into_iter().enumerate() {
        let created = emulator
            .ok(
                "CreateStateMachine",
                &json!({
                    "name": format!("express-child-{index}"),
                    "definition": child_definition,
                    "roleArn": ROLE_ARN,
                    "type": "EXPRESS",
                }),
            )
            .await;
        let child = string(&created, "stateMachineArn");
        let definition = parent_definition(resource, &child, json!({ "mode": "run" }));
        let parent = create_machine(&emulator, &format!("express-parent-{index}"), &definition)
            .await;
        let started = emulator
            .ok(
                "StartExecution",
                &json!({ "stateMachineArn": parent, "name": format!("express-{index}") }),
            )
            .await;
        let execution = string(&started, "executionArn");

        let described = settled(&emulator, case, &execution).await;
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some(expected),
            "case {case:?}: expected the parent to end {expected}, got {described:?}"
        );
        if expected != "SUCCEEDED" {
            // The child's failure is the task's, reported on the task's own event.
            let failed = event(&emulator, &execution, EVENT_TASK_FAILED)
                .await
                .unwrap_or_else(|| panic!("case {case:?}: expected a {EVENT_TASK_FAILED} event"));
            let cause = failed
                .get("taskFailedEventDetails")
                .and_then(|details| details.get("cause"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            assert!(
                cause.contains("Child.Refused"),
                "case {case:?}: expected the child's own error in the cause, got {cause:?}"
            );
            continue;
        }
        let succeeded = event(&emulator, &execution, EVENT_TASK_SUCCEEDED)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: expected a {EVENT_TASK_SUCCEEDED} event"));
        let reported = document(
            succeeded
                .get("taskSucceededEventDetails")
                .and_then(|details| details.get("output"))
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?}: expected the task's output")),
        );
        assert_eq!(
            reported.get("Status").and_then(Value::as_str),
            Some("SUCCEEDED"),
            "case {case:?}: expected the child's own status in the result, got {reported}"
        );
        for member in ["ExecutionArn", "StateMachineArn", "Name", "Output"] {
            assert!(
                reported.get(member).is_some(),
                "case {case:?}: expected the member {member:?} of the child's record, got \
                 {reported}"
            );
        }
    }
}

/// A nested-execution task whose STARTING call the service refused records the task's
/// own failure on that resource and claims no submission.
///
/// The rows vary what the service refuses the start for — a child it does not hold, and
/// a request naming no child at all — the dimensions a refused start generalises over.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_start_records_the_tasks_own_failure() {
    let absent = format!("arn:aws:states:{REGION}:000000000000:stateMachine:no-such-child");
    // (case, the task's parameters, the error the parent must report)
    let rows: [(&str, Value, &str); 2] = [
        (
            "a child the service does not hold",
            json!({ "StateMachineArn": absent, "Input": {} }),
            "StepFunctions.StateMachineDoesNotExistException",
        ),
        (
            "a request naming no child at all",
            json!({ "Input": {} }),
            "StepFunctions.ValidationException",
        ),
    ];

    let emulator = Emulator::new();
    for (index, (case, parameters, expected)) in rows.into_iter().enumerate() {
        let definition = json!({
            "StartAt": "RunChild",
            "States": {
                "RunChild": {
                    "Type": "Task",
                    "Resource": "arn:aws:states:::states:startExecution.sync:2",
                    "Parameters": parameters,
                    "End": true,
                }
            }
        })
        .to_string();
        let parent = create_machine(&emulator, &format!("sync-refused-{index}"), &definition).await;
        let started = emulator
            .ok(
                "StartExecution",
                &json!({ "stateMachineArn": parent, "name": format!("refused-{index}") }),
            )
            .await;
        let execution = string(&started, "executionArn");

        let described = settled(&emulator, case, &execution).await;
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some("FAILED"),
            "case {case:?}: expected the parent to fail, got {described:?}"
        );
        assert_eq!(
            described.get("error").and_then(Value::as_str),
            Some(expected),
            "case {case:?}: expected {expected}, got {described:?}"
        );
        let (events, _) = page(
            &emulator,
            &json!({ "executionArn": execution.clone(), "maxResults": 1000 }),
        )
        .await;
        let failures: Vec<&Value> = events
            .iter()
            .filter(|event| event.get("type").and_then(Value::as_str) == Some(EVENT_TASK_FAILED))
            .collect();
        assert_eq!(
            failures.len(),
            1,
            "case {case:?}: expected exactly one {EVENT_TASK_FAILED}, got {failures:?}"
        );
        assert_eq!(
            failures[0]
                .get("taskFailedEventDetails")
                .and_then(|details| details.get("resource"))
                .and_then(Value::as_str),
            Some("startExecution.sync:2"),
            "case {case:?}: expected the failure on the state's own resource, got {:?}",
            failures[0]
        );
        assert!(
            event(&emulator, &execution, EVENT_TASK_SUCCEEDED)
                .await
                .is_none(),
            "case {case:?}: nothing was run, so no {EVENT_TASK_SUCCEEDED} event may appear"
        );
    }
}
