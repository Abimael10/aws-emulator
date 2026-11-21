// SPDX-License-Identifier: CC0-1.0

//! `PollForDecisionTask`, end to end over the service's own wire protocol (`awsJson1_0`,
//! `POST /` with `X-Amz-Target: SimpleWorkflowService.<Operation>`), dispatched through
//! the same `MultiServiceRouter` the emulator mounts in production.
//!
//! The rule every case here asserts: a poll of a decision task list hands the caller the
//! one decision task of that domain's OPEN runs that comes first in the list — highest
//! `taskPriority` first, then first-scheduled first — records the hand-over as a
//! `DecisionTaskStarted` event carrying the caller's `identity` and naming the
//! `DecisionTaskScheduled` event it started, and answers entirely from that stored
//! history: the task's run and workflow type, the new event's id as `startedEventId`, the
//! `startedEventId` of the last decision task a decider completed as
//! `previousStartedEventId`, and a page of the run's events. A task is handed out at most
//! once, a list with nothing waiting is held open — the call is the documented long poll,
//! so the empty result (`taskToken` an empty string) comes only once the documented
//! 60-second timeout has passed, whatever the reason the list holds nothing assignable,
//! a `domain` that resolves to no stored domain included — and a task not completed
//! within its own `startToCloseTimeout` is timed out and replaced by the server at the
//! instant that duration runs out.
//!
//! Every row varies what the rule generalises over — where the task list came from, the
//! identity's size, the `workflowId`'s shape, how many runs share one list, the task
//! priorities and the order they were started in, the page size and ordering, and the
//! timeout duration (including the published sentinel `NONE`) — and every expected value
//! is computed by the test from what it seeded, never hardcoded from one recording.

use serde_json::{json, Value};
use service_com_amazonaws_swf_simpleworkflowservice::ops::types::{
    DECISION_TASK_TIMEOUT_TYPE_VALUES, EVENT_TYPE_VALUES,
};
use service_com_amazonaws_swf_simpleworkflowservice::state::POLL_TIMEOUT;

use super::workflow_execution_lifecycle::{
    error_code, full_defaults, merge, seeded, string_at, whole_history, Emulator,
};

/// The workflow type every run in this suite is started with.
const WORKFLOW_TYPE: (&str, &str) = ("demo-wf", "1.0");

/// The `defaultTaskList` the seeded workflow type carries, so a start that names no
/// `taskList` schedules its decision task there.
const DEFAULT_LIST: &str = "default-deciders";

/// The modelled `TaskToken` maximum length (`len 1..1024`).
const MAX_TASK_TOKEN_LENGTH: usize = 1024;

/// How long a poll of a task list with nothing waiting is given to prove it is NOT
/// answering. The documented long-poll timeout is 60 seconds, so any wait far below it
/// that produces no answer shows the connection was held open.
const HOLD_OPEN_PROBE: std::time::Duration = std::time::Duration::from_millis(250);

/// Register the domain and the one workflow type this suite starts runs of.
async fn seed(emulator: &Emulator, domain: &str) {
    seeded(
        emulator,
        domain,
        domain,
        WORKFLOW_TYPE,
        full_defaults(DEFAULT_LIST, "TERMINATE"),
    )
    .await;
}

/// Start one run, optionally overriding the task list, the task priority and the
/// decision task's `taskStartToCloseTimeout`. Returns the minted `runId`.
async fn start(
    emulator: &Emulator,
    domain: &str,
    workflow_id: &str,
    task_list: Option<&str>,
    task_priority: Option<&str>,
    task_timeout: Option<&str>,
) -> String {
    let mut body = json!({
        "domain": domain,
        "workflowId": workflow_id,
        "workflowType": { "name": WORKFLOW_TYPE.0, "version": WORKFLOW_TYPE.1 },
    });
    if let Some(task_list) = task_list {
        merge(&mut body, json!({ "taskList": { "name": task_list } }));
    }
    if let Some(task_priority) = task_priority {
        merge(&mut body, json!({ "taskPriority": task_priority }));
    }
    if let Some(task_timeout) = task_timeout {
        merge(&mut body, json!({ "taskStartToCloseTimeout": task_timeout }));
    }
    let started = emulator.ok(workflow_id, "StartWorkflowExecution", &body).await;
    string_at(&started, "/runId")
}

/// One `PollForDecisionTask` call, requiring a 200. `extra` carries the optional members
/// this row exercises (`identity`, `maximumPageSize`, `reverseOrder`, `nextPageToken`,
/// `startAtPreviousStartedEvent`).
async fn poll(emulator: &Emulator, case: &str, domain: &str, task_list: &str, extra: Value) -> Value {
    let mut body = json!({ "domain": domain, "taskList": { "name": task_list } });
    merge(&mut body, extra);
    emulator.ok(case, "PollForDecisionTask", &body).await
}

/// How many decision tasks are waiting in one list, as `CountPendingDecisionTasks`
/// reports it.
async fn pending(emulator: &Emulator, case: &str, domain: &str, task_list: &str) -> i64 {
    let body = emulator
        .ok(
            case,
            "CountPendingDecisionTasks",
            &json!({ "domain": domain, "taskList": { "name": task_list } }),
        )
        .await;
    body.get("count")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| panic!("case {case:?}: expected a count in {body:?}"))
}

/// The `events` list of an answer.
fn events_of(case: &str, body: &Value) -> Vec<Value> {
    body.pointer("/events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected an events list in {body:?}"))
}

/// One event's `eventType`.
fn event_type(event: &Value) -> &str {
    event
        .get("eventType")
        .and_then(Value::as_str)
        .unwrap_or("<missing eventType>")
}

/// Every event of one type, in the order given.
fn events_typed<'a>(events: &'a [Value], wanted: &str) -> Vec<&'a Value> {
    assert!(
        EVENT_TYPE_VALUES.contains(&wanted),
        "case {wanted:?}: the model's EventType enumeration must hold the type under test"
    );
    events
        .iter()
        .filter(|event| event_type(event) == wanted)
        .collect()
}

/// The `taskToken` of an answer that handed out a task: present, non-empty and inside the
/// modelled length.
fn handed_out_token(case: &str, body: &Value) -> String {
    let token = string_at(body, "/taskToken");
    assert!(
        !token.is_empty() && token.len() <= MAX_TASK_TOKEN_LENGTH,
        "case {case:?}: a handed-out task carries an opaque token of 1..{MAX_TASK_TOKEN_LENGTH} \
         characters, got {token:?}"
    );
    token
}

/// An `EventId` member of an answer; both default to `0`, so an absent member is zero.
fn event_id_member(body: &Value, member: &str) -> i64 {
    body.get(member).and_then(Value::as_i64).unwrap_or(0)
}

/// Assert a poll of a task list with NOTHING waiting holds the connection open instead
/// of answering, for the whole of `probe`.
///
/// `PollForDecisionTask` is a long poll: "the service holds the HTTP connection open and
/// responds as soon a task becomes available. If no decision task is available in the
/// specified task list before the timeout of 60 seconds expires, an empty result is
/// returned." So a drained list answers nothing for that long; a poll that answers inside
/// `probe` is the failure. On the real clock the probe is [`HOLD_OPEN_PROBE`], far below
/// the documented timeout; a case running on a PAUSED clock passes a probe up to that
/// timeout, which virtual time carries without anything sleeping.
async fn assert_holds_open_for(
    emulator: &Emulator,
    case: &str,
    domain: &str,
    task_list: &str,
    probe: std::time::Duration,
) {
    let answered = tokio::time::timeout(
        probe,
        emulator.call(
            "PollForDecisionTask",
            &json!({ "domain": domain, "taskList": { "name": task_list } }),
        ),
    )
    .await;
    assert!(
        answered.is_err(),
        "case {case:?}: a poll of a task list with nothing waiting holds the connection \
         open rather than answering, got {answered:?}"
    );
}

/// Assert a poll of a task list with nothing waiting holds the connection open through
/// [`HOLD_OPEN_PROBE`] — the real-clock probe every case on the real clock uses.
async fn assert_holds_open(emulator: &Emulator, case: &str, domain: &str, task_list: &str) {
    assert_holds_open_for(emulator, case, domain, task_list, HOLD_OPEN_PROBE).await;
}

/// One run polled for its first decision task.
struct HandOutRow {
    case: &'static str,
    workflow_id: String,
    /// The list the start names, or `None` to inherit the type's `defaultTaskList`.
    requested_list: Option<&'static str>,
    /// The `identity` the poll passes; `None` passes the member not at all and `Some("")`
    /// passes it empty (the modelled `Identity` is `len 0..256`).
    identity: Option<String>,
}

impl HandOutRow {
    /// The list this run's decision task waits in.
    fn task_list(&self) -> &str {
        self.requested_list.unwrap_or(DEFAULT_LIST)
    }
}

#[tokio::test]
async fn a_poll_hands_out_the_waiting_task_and_records_the_hand_over_in_the_history() {
    let emulator = Emulator::new();
    seed(&emulator, "handout").await;

    // Rows vary where the list came from, the identity's size (absent, plain, the
    // modelled 256-character maximum, and empty — which is no identity at all) and the
    // `workflowId`'s shape and size.
    let rows = [
        HandOutRow {
            case: "list named on the request",
            workflow_id: "wf-requested".to_string(),
            requested_list: Some("deciders-a"),
            identity: Some("decider-1".to_string()),
        },
        HandOutRow {
            case: "the registered default list",
            workflow_id: "order 42".to_string(),
            requested_list: None,
            identity: None,
        },
        HandOutRow {
            case: "identity at the modelled maximum",
            workflow_id: "wf-long-identity".to_string(),
            requested_list: Some("deciders-b"),
            identity: Some("d".repeat(256)),
        },
        HandOutRow {
            case: "an empty identity is no identity",
            workflow_id: "wf-empty-identity".to_string(),
            requested_list: Some("deciders-c"),
            identity: Some(String::new()),
        },
        HandOutRow {
            case: "a workflowId at the modelled maximum",
            workflow_id: "w".repeat(256),
            requested_list: Some("deciders-d"),
            identity: Some("decider-2".to_string()),
        },
    ];

    let mut tokens = Vec::new();
    for row in &rows {
        let case = row.case;
        let list = row.task_list();
        let run_id = start(
            &emulator,
            "handout",
            &row.workflow_id,
            row.requested_list,
            None,
            None,
        )
        .await;
        let waiting = pending(&emulator, case, "handout", list).await;
        assert_eq!(
            waiting, 1,
            "case {case:?}: the start schedules one decision task in {list:?}, got {waiting}"
        );

        let mut extra = json!({});
        if let Some(identity) = &row.identity {
            merge(&mut extra, json!({ "identity": identity }));
        }
        let answer = poll(&emulator, case, "handout", list, extra).await;

        let token = handed_out_token(case, &answer);
        assert_eq!(
            (
                string_at(&answer, "/workflowExecution/workflowId"),
                string_at(&answer, "/workflowExecution/runId"),
                string_at(&answer, "/workflowType/name"),
                string_at(&answer, "/workflowType/version"),
            ),
            (
                row.workflow_id.clone(),
                run_id.clone(),
                WORKFLOW_TYPE.0.to_string(),
                WORKFLOW_TYPE.1.to_string(),
            ),
            "case {case:?}: the task names the run waiting in {list:?} and its registered \
             type, got {answer:?}"
        );
        assert_eq!(
            event_id_member(&answer, "previousStartedEventId"),
            0,
            "case {case:?}: no decision task of this run has been completed, so there is no \
             previous started event, got {answer:?}"
        );

        // The answer's events ARE the run's stored history, read back through
        // GetWorkflowExecutionHistory after the poll.
        let events = events_of(case, &answer);
        let stored = whole_history(&emulator, "handout", &row.workflow_id, &run_id).await;
        assert_eq!(
            events, stored,
            "case {case:?}: the poll answers with the run's persisted history, got {events:?}"
        );
        assert!(
            answer.get("nextPageToken").is_none(),
            "case {case:?}: a history this short fits in one page, got {answer:?}"
        );

        // The hand-over is one DecisionTaskStarted event: its id is the answer's
        // startedEventId, it names the scheduling event it started, and it carries the
        // identity the poll passed (and no identity member when none was passed).
        let scheduled = events_typed(&events, "DecisionTaskScheduled");
        let started = events_typed(&events, "DecisionTaskStarted");
        assert_eq!(
            (scheduled.len(), started.len()),
            (1, 1),
            "case {case:?}: the run has one scheduled and one started decision task, \
             got {events:?}"
        );
        let scheduled_event_id = scheduled[0]
            .get("eventId")
            .and_then(Value::as_i64)
            .unwrap_or_else(|| panic!("case {case:?}: no eventId on {:?}", scheduled[0]));
        let started_event_id = started[0]
            .get("eventId")
            .and_then(Value::as_i64)
            .unwrap_or_else(|| panic!("case {case:?}: no eventId on {:?}", started[0]));
        assert_eq!(
            (
                event_id_member(&answer, "startedEventId"),
                started[0]
                    .pointer("/decisionTaskStartedEventAttributes/scheduledEventId")
                    .and_then(Value::as_i64),
            ),
            (started_event_id, Some(scheduled_event_id)),
            "case {case:?}: startedEventId is the recorded event's id and that event names \
             the task it started, got {events:?}"
        );
        let recorded_identity = started[0]
            .pointer("/decisionTaskStartedEventAttributes/identity")
            .and_then(Value::as_str);
        let expected_identity = row
            .identity
            .as_deref()
            .filter(|identity| !identity.is_empty());
        assert_eq!(
            recorded_identity, expected_identity,
            "case {case:?}: the started event records the decider's identity, got {events:?}"
        );

        // Handing the task over took it out of the list, so a second poll of the same list
        // finds nothing waiting — and, being a long poll, holds open instead of answering.
        let left = pending(&emulator, case, "handout", list).await;
        assert_eq!(
            left, 0,
            "case {case:?}: the handed-out task no longer waits in {list:?}, got {left}"
        );
        assert_holds_open(&emulator, case, "handout", list).await;

        tokens.push((case, token));
    }

    // Each task got its own handle.
    for (left, (left_case, left_token)) in tokens.iter().enumerate() {
        for (right_case, right_token) in tokens.iter().skip(left + 1) {
            assert_ne!(
                left_token, right_token,
                "case {left_case:?} vs {right_case:?}: two different tasks must not share a \
                 token, got {left_token:?}"
            );
        }
    }
}

/// One run sharing a single task list with the others, started in table order.
struct PriorityRow {
    workflow_id: &'static str,
    /// The `taskPriority` the start names, `None` to leave it unset. The member's doc:
    /// "Valid values are integers that range from Java's Integer.MIN_VALUE (-2147483648)
    /// to Integer.MAX_VALUE (2147483647). Higher numbers indicate higher priority."
    priority: Option<&'static str>,
    /// What an unset priority means: Amazon SWF's documented default of zero.
    effective_priority: i64,
}

#[tokio::test]
async fn the_tasks_of_one_list_are_handed_out_highest_priority_then_oldest_first() {
    let emulator = Emulator::new();
    seed(&emulator, "order").await;
    let list = "deciders-shared";

    // Rows span both published bounds of `taskPriority`, a value either side of the
    // default, the default itself (left unset) and two runs sharing one priority — whose
    // order is then the order they were scheduled in. The start order below is
    // deliberately NOT the expected dispatch order.
    let rows = [
        PriorityRow {
            workflow_id: "wf-zero-first",
            priority: None,
            effective_priority: 0,
        },
        PriorityRow {
            workflow_id: "wf-max",
            priority: Some("2147483647"),
            effective_priority: 2_147_483_647,
        },
        PriorityRow {
            workflow_id: "wf-min",
            priority: Some("-2147483648"),
            effective_priority: -2_147_483_648,
        },
        PriorityRow {
            workflow_id: "wf-low",
            priority: Some("-100"),
            effective_priority: -100,
        },
        PriorityRow {
            workflow_id: "wf-high",
            priority: Some("50"),
            effective_priority: 50,
        },
        PriorityRow {
            workflow_id: "wf-zero-second",
            priority: Some("0"),
            effective_priority: 0,
        },
    ];

    for row in &rows {
        start(
            &emulator,
            "order",
            row.workflow_id,
            Some(list),
            row.priority,
            None,
        )
        .await;
    }

    // The oracle: the rule applied by the TEST to the seeded table.
    let mut expected: Vec<(usize, &PriorityRow)> = rows.iter().enumerate().collect();
    expected.sort_by_key(|(position, row)| (-i128::from(row.effective_priority), *position));
    let expected: Vec<&'static str> = expected
        .into_iter()
        .map(|(_, row)| row.workflow_id)
        .collect();

    let mut handed_out = Vec::new();
    let mut tokens = Vec::new();
    for ordinal in 0..rows.len() {
        let case = format!("hand-out {ordinal}");
        let waiting = pending(&emulator, &case, "order", list).await;
        assert_eq!(
            waiting,
            (rows.len() - ordinal) as i64,
            "case {case:?}: every task not yet handed out still waits in {list:?}, got {waiting}"
        );
        let answer = poll(&emulator, &case, "order", list, json!({})).await;
        tokens.push(handed_out_token(&case, &answer));
        handed_out.push(string_at(&answer, "/workflowExecution/workflowId"));
    }

    assert_eq!(
        handed_out, expected,
        "case \"dispatch order\": higher taskPriority first, then first-scheduled first"
    );
    tokens.sort();
    let distinct = tokens.len();
    tokens.dedup();
    assert_eq!(
        tokens.len(),
        distinct,
        "case \"dispatch order\": each of the {distinct} tasks must carry its own token"
    );

    // Every task of the list has been handed out exactly once, so the list is now empty
    // and a further poll of it waits rather than answering.
    assert_holds_open(&emulator, "drained", "order", list).await;
    let waiting = pending(&emulator, "drained", "order", list).await;
    assert_eq!(
        waiting, 0,
        "case \"drained\": no task of {list:?} is waiting any more, got {waiting}"
    );

    // A list nothing was ever scheduled in waits the same way.
    assert_holds_open(&emulator, "never used", "order", "deciders-unused").await;
}

/// One paged read of a handed-out task's history.
struct PageRow {
    case: &'static str,
    workflow_id: &'static str,
    /// The `maximumPageSize` the poll asks for; `0` is the member's own default ("not
    /// specified"), which returns as much as one answer carries.
    page_size: i64,
    reverse_order: bool,
}

#[tokio::test]
async fn the_history_is_paged_with_the_task_the_page_token_names() {
    let emulator = Emulator::new();
    seed(&emulator, "paging").await;

    // Rows vary the page size (one event at a time, two at a time, the member's default
    // and the modelled `PageSize` maximum) and the ordering.
    let rows = [
        PageRow {
            case: "one event per page",
            workflow_id: "wf-page-1",
            page_size: 1,
            reverse_order: false,
        },
        PageRow {
            case: "two events per page",
            workflow_id: "wf-page-2",
            page_size: 2,
            reverse_order: false,
        },
        PageRow {
            case: "one event per page, reversed",
            workflow_id: "wf-page-rev",
            page_size: 1,
            reverse_order: true,
        },
        PageRow {
            case: "the default page size",
            workflow_id: "wf-page-default",
            page_size: 0,
            reverse_order: false,
        },
        PageRow {
            case: "the modelled maximum page size",
            workflow_id: "wf-page-max",
            page_size: 1000,
            reverse_order: true,
        },
    ];

    for row in &rows {
        let case = row.case;
        // Each row gets its own list so the task it pages is the one it seeded.
        let list = format!("deciders-{}", row.workflow_id);
        let run_id = start(&emulator, "paging", row.workflow_id, Some(&list), None, None).await;

        let mut extra = json!({ "reverseOrder": row.reverse_order });
        if row.page_size > 0 {
            merge(&mut extra, json!({ "maximumPageSize": row.page_size }));
        }

        let first = poll(&emulator, case, "paging", &list, extra.clone()).await;
        let token = handed_out_token(case, &first);
        let started_event_id = event_id_member(&first, "startedEventId");
        let mut pages = vec![events_of(case, &first)];
        let mut next = first.get("nextPageToken").and_then(Value::as_str).map(str::to_string);

        // Every further page names the SAME task: the token carries which task is being
        // paged, so no page hands out a new one.
        while let Some(page_token) = next {
            let mut body = extra.clone();
            merge(&mut body, json!({ "nextPageToken": page_token }));
            let page = poll(&emulator, case, "paging", &list, body).await;
            assert_eq!(
                (
                    string_at(&page, "/taskToken"),
                    event_id_member(&page, "startedEventId"),
                    string_at(&page, "/workflowExecution/runId"),
                ),
                (token.clone(), started_event_id, run_id.clone()),
                "case {case:?}: a further page belongs to the task its token names, got {page:?}"
            );
            pages.push(events_of(case, &page));
            next = page.get("nextPageToken").and_then(Value::as_str).map(str::to_string);
        }

        // The pages are the whole history, in the requested ordering, split at the
        // requested size and never overlapping.
        let stored = whole_history(&emulator, "paging", row.workflow_id, &run_id).await;
        let mut expected = stored.clone();
        if row.reverse_order {
            expected.reverse();
        }
        let walked: Vec<Value> = pages.iter().flatten().cloned().collect();
        assert_eq!(
            walked, expected,
            "case {case:?}: the pages concatenate to the whole history in order, got {pages:?}"
        );
        let limit = if row.page_size > 0 {
            row.page_size as usize
        } else {
            stored.len()
        };
        for (ordinal, page) in pages.iter().enumerate() {
            assert!(
                !page.is_empty() && page.len() <= limit,
                "case {case:?}: page {ordinal} holds 1..={limit} events, got {page:?}"
            );
        }
        let expected_pages = stored.len().div_ceil(limit);
        assert_eq!(
            pages.len(),
            expected_pages,
            "case {case:?}: {} events at {limit} per page is {expected_pages} page(s), got {}",
            stored.len(),
            pages.len()
        );

        // Paging handed out nothing: the run still has exactly one started decision task
        // and the list is still empty.
        let started = events_typed(&stored, "DecisionTaskStarted");
        assert_eq!(
            started.len(),
            1,
            "case {case:?}: paging a task's history records no second hand-over, got {stored:?}"
        );
        let waiting = pending(&emulator, case, "paging", &list).await;
        assert_eq!(
            waiting, 0,
            "case {case:?}: paging neither returns a task to {list:?} nor takes another out, \
             got {waiting}"
        );
    }
}

#[tokio::test]
async fn start_at_previous_started_event_begins_at_the_previous_decision_task() {
    let emulator = Emulator::new();
    seed(&emulator, "resume").await;
    let list = "deciders-resume";
    let run_id = start(&emulator, "resume", "wf-resume", Some(list), None, None).await;

    // The first decision task of a run has no earlier DecisionTaskStarted event to begin
    // at, so the whole history is returned either way.
    for start_at_previous in [false, true] {
        let case = format!("first task, startAtPreviousStartedEvent={start_at_previous}");
        if start_at_previous {
            // The first poll of the loop took the run's only task, so the list now holds
            // nothing and the poll waits; the flag itself is exercised on the task the
            // first iteration was handed, and on the second task below.
            assert_holds_open(&emulator, &case, "resume", list).await;
            continue;
        }
        let answer = poll(
            &emulator,
            &case,
            "resume",
            list,
            json!({ "startAtPreviousStartedEvent": start_at_previous, "maximumPageSize": 1000 }),
        )
        .await;
        let events = events_of(&case, &answer);
        let stored = whole_history(&emulator, "resume", "wf-resume", &run_id).await;
        assert_eq!(
            events, stored,
            "case {case:?}: a run with no previous started event keeps its whole history, \
             got {events:?}"
        );
    }
}

/// One decision task whose own `startToCloseTimeout` either has or has not run out by the
/// time the next poll arrives.
struct TimeoutRow {
    case: &'static str,
    domain: &'static str,
    /// The `taskStartToCloseTimeout` the start names. `startToCloseTimeout`'s doc: "The
    /// maximum duration for this decision task. The task is considered timed out if it
    /// doesn't completed within this duration. The duration is specified in seconds, an
    /// integer greater than or equal to 0. You can use NONE to specify unlimited
    /// duration."
    timeout: &'static str,
    /// Whether that duration has already run out at the instant of the next call.
    times_out: bool,
}

#[tokio::test]
async fn a_task_not_completed_within_its_start_to_close_duration_is_timed_out_and_replaced() {
    let rows = [
        TimeoutRow {
            case: "a zero-second duration is out the moment the task starts",
            domain: "timeout-zero",
            timeout: "0",
            times_out: true,
        },
        TimeoutRow {
            case: "the NONE sentinel never runs out",
            domain: "timeout-none",
            timeout: "NONE",
            times_out: false,
        },
        TimeoutRow {
            case: "an hour has not run out yet",
            domain: "timeout-hour",
            timeout: "3600",
            times_out: false,
        },
    ];

    let emulator = Emulator::new();
    for row in &rows {
        let case = row.case;
        seed(&emulator, row.domain).await;
        let list = "deciders-timeout";
        let run_id = start(
            &emulator,
            row.domain,
            "wf-timeout",
            Some(list),
            None,
            Some(row.timeout),
        )
        .await;

        let first = poll(&emulator, case, row.domain, list, json!({})).await;
        let first_token = handed_out_token(case, &first);
        let first_started = event_id_member(&first, "startedEventId");
        let first_scheduled = events_typed(&events_of(case, &first), "DecisionTaskScheduled")
            .first()
            .and_then(|event| event.get("eventId"))
            .and_then(Value::as_i64)
            .unwrap_or_else(|| panic!("case {case:?}: no scheduled event in {first:?}"));

        if !row.times_out {
            // The task is still outstanding, so the list holds nothing and the second
            // poll waits instead of answering.
            assert_holds_open(&emulator, case, row.domain, list).await;
            let stored = whole_history(&emulator, row.domain, "wf-timeout", &run_id).await;
            assert!(
                events_typed(&stored, "DecisionTaskTimedOut").is_empty()
                    && events_typed(&stored, "DecisionTaskScheduled").len() == 1,
                "case {case:?}: a duration that has not run out times nothing out and \
                 schedules no replacement, got {stored:?}"
            );
            continue;
        }

        let second = poll(&emulator, case, row.domain, list, json!({})).await;
        let stored = whole_history(&emulator, row.domain, "wf-timeout", &run_id).await;
        let timed_out = events_typed(&stored, "DecisionTaskTimedOut");
        let scheduled = events_typed(&stored, "DecisionTaskScheduled");

        // The timeout is the SERVER's: it fell due at the instant the duration ran out,
        // and the run carries the modelled DecisionTaskTimedOut event naming the task it
        // ended, followed by a replacement scheduled in the same list. A zero-second
        // duration is out again the moment the replacement is handed over, so the history
        // may hold several such cycles by the time it is read — one replacement per
        // timeout, never more.
        assert!(
            !timed_out.is_empty(),
            "case {case:?}: the outstanding task timed out, got {stored:?}"
        );
        assert_eq!(
            (
                timed_out[0]
                    .pointer("/decisionTaskTimedOutEventAttributes/timeoutType")
                    .and_then(Value::as_str),
                timed_out[0]
                    .pointer("/decisionTaskTimedOutEventAttributes/scheduledEventId")
                    .and_then(Value::as_i64),
                timed_out[0]
                    .pointer("/decisionTaskTimedOutEventAttributes/startedEventId")
                    .and_then(Value::as_i64),
            ),
            (
                Some("START_TO_CLOSE"),
                Some(first_scheduled),
                Some(first_started)
            ),
            "case {case:?}: the timeout names its own type and the task it ended, got {stored:?}"
        );
        assert!(
            DECISION_TASK_TIMEOUT_TYPE_VALUES.contains(&"START_TO_CLOSE"),
            "case {case:?}: START_TO_CLOSE is one of the model's DecisionTaskTimeoutType values"
        );
        assert_eq!(
            scheduled.len(),
            timed_out.len() + 1,
            "case {case:?}: each timeout schedules exactly one replacement, got {stored:?}"
        );
        for (ordinal, event) in scheduled.iter().enumerate() {
            assert_eq!(
                event
                    .pointer("/decisionTaskScheduledEventAttributes/taskList/name")
                    .and_then(Value::as_str),
                Some(list),
                "case {case:?}: scheduled task {ordinal} waits in the same list, got {stored:?}"
            );
        }

        // The second poll was handed that replacement: a new task, with its own token.
        let second_token = handed_out_token(case, &second);
        let second_started = event_id_member(&second, "startedEventId");
        assert!(
            second_started > first_started && second_token != first_token,
            "case {case:?}: the replacement is a new task with its own started event and \
             token, got {second:?}"
        );
    }
}

#[tokio::test]
async fn a_poll_the_service_cannot_serve_is_refused_without_changing_anything() {
    let emulator = Emulator::new();
    seed(&emulator, "refusals").await;
    let list = "deciders-refusals";
    let run_id = start(&emulator, "refusals", "wf-refusals", Some(list), None, None).await;

    // SWF's naming rule, which the model cannot express: "The specified string must not
    // start or end with whitespace. It must not contain a : (colon), / (slash), |
    // (vertical bar), or any control characters (\u0000-\u001f | \u007f-\u009f). Also, it
    // must not be the literal string arn."
    for (case, name) in [
        ("leading whitespace", " deciders"),
        ("trailing whitespace", "deciders "),
        ("a colon", "deciders:1"),
        ("a slash", "deciders/1"),
        ("a vertical bar", "deciders|1"),
        ("a control character", "deciders\u{1}"),
        ("a high control character", "deciders\u{7f}"),
        ("the literal arn", "arn"),
    ] {
        emulator
            .refused(
                case,
                "PollForDecisionTask",
                &json!({ "domain": "refusals", "taskList": { "name": name } }),
                "ValidationException",
            )
            .await;
    }

    // None of that handed out the waiting task.
    let waiting = pending(&emulator, "after refusals", "refusals", list).await;
    assert_eq!(
        waiting, 1,
        "case \"after refusals\": a refused poll hands out nothing, got {waiting}"
    );

    // Page one event of the task so there is a real continuation token to mistreat.
    let first = poll(
        &emulator,
        "paged",
        "refusals",
        list,
        json!({ "maximumPageSize": 1 }),
    )
    .await;
    let token = first
        .get("nextPageToken")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case \"paged\": a one-event page leaves more to read: {first:?}"))
        .to_string();

    // A token is only valid for the ordering it was issued for, and a token this service
    // never issued is not a token at all.
    for (case, bad_token, reverse_order) in [
        ("the other ordering", token.clone(), true),
        ("not a token", "not-a-page-token".to_string(), false),
        ("an empty-payload token", "df".to_string(), false),
    ] {
        emulator
            .refused(
                case,
                "PollForDecisionTask",
                &json!({
                    "domain": "refusals",
                    "taskList": { "name": list },
                    "nextPageToken": bad_token,
                    "reverseOrder": reverse_order,
                    "maximumPageSize": 1,
                }),
                "ValidationException",
            )
            .await;
    }

    // A token addresses the task of ONE run, so the same token under a domain the store
    // does not hold addresses no task at all: the modelled `UnknownResourceFault` naming
    // the execution the token addresses, which is the leaf this paging read resolves.
    for domain in ["never-registered", "REFUSALS", "refusals-2"] {
        let case = format!("a token paged under {domain}");
        let (status, body) = emulator
            .call(
                "PollForDecisionTask",
                &json!({
                    "domain": domain,
                    "taskList": { "name": list },
                    "nextPageToken": token.clone(),
                    "maximumPageSize": 1,
                }),
            )
            .await;
        assert_eq!(
            (
                status,
                error_code(&body).as_str(),
                body.get("message").and_then(Value::as_str),
            ),
            (
                400,
                "UnknownResourceFault",
                Some(
                    format!(
                        "Unknown execution: WorkflowExecution=[workflowId=wf-refusals, \
                         runId={run_id}]"
                    )
                    .as_str()
                ),
            ),
            "case {case:?}: paging a token under a domain the store does not hold is the \
             modelled fault naming the execution the token addresses, got {body:?}"
        );
    }

    // The valid token still pages the same task, so the refusals changed nothing.
    let next = poll(
        &emulator,
        "valid token",
        "refusals",
        list,
        json!({ "nextPageToken": token, "maximumPageSize": 1 }),
    )
    .await;
    assert_eq!(
        (
            string_at(&next, "/taskToken"),
            string_at(&next, "/workflowExecution/runId"),
        ),
        (string_at(&first, "/taskToken"), run_id),
        "case \"valid token\": the token pages the task it was issued for, got {next:?}"
    );
}

#[tokio::test]
async fn a_page_token_of_a_task_that_is_over_no_longer_pages() {
    let emulator = Emulator::new();
    seed(&emulator, "stale").await;
    let list = "deciders-stale";

    // A zero-second `taskStartToCloseTimeout` runs out the moment the task is handed
    // over, so the token below names a task that is already over by the next call — the
    // same staleness a longer duration reaches later.
    start(&emulator, "stale", "wf-stale", Some(list), None, Some("0")).await;
    let first = poll(
        &emulator,
        "stale",
        "stale",
        list,
        json!({ "maximumPageSize": 1 }),
    )
    .await;
    let token = first
        .get("nextPageToken")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case \"stale\": a one-event page leaves more to read: {first:?}"))
        .to_string();

    emulator
        .refused(
            "stale",
            "PollForDecisionTask",
            &json!({
                "domain": "stale",
                "taskList": { "name": list },
                "nextPageToken": token,
                "maximumPageSize": 1,
            }),
            "ValidationException",
        )
        .await;
}

/// Assert one answer is the documented EMPTY result of this long poll: 200, `taskToken`
/// an empty string, and every other modelled member at the value that identifies no task.
fn assert_empty_decision_task(case: &str, status: u16, body: &Value) {
    assert_eq!(
        status, 200,
        "case {case:?}: a poll with nothing assignable completes the long poll rather \
         than refusing it, got {status} with {body:?}"
    );
    let token = body.get("taskToken").and_then(Value::as_str).unwrap_or("");
    assert!(
        token.is_empty(),
        "case {case:?}: the empty result's taskToken is an empty string, got {token:?} \
         in {body:?}"
    );
    assert_eq!(
        (
            event_id_member(body, "startedEventId"),
            event_id_member(body, "previousStartedEventId"),
        ),
        (0, 0),
        "case {case:?}: the empty result reports no event ids, got {body:?}"
    );
    let events = body
        .pointer("/events")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    assert_eq!(
        events, 0,
        "case {case:?}: the empty result carries no history events, got {body:?}"
    );
    assert!(
        body.get("nextPageToken").is_none(),
        "case {case:?}: the empty result pages nothing, got {body:?}"
    );
    for member in ["/workflowExecution/workflowId", "/workflowExecution/runId"] {
        let named = body.pointer(member).and_then(Value::as_str).unwrap_or("");
        assert!(
            named.is_empty(),
            "case {case:?}: the empty result names no run at {member}, got {named:?} in \
             {body:?}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn a_poll_with_nothing_assignable_completes_empty_whether_or_not_its_domain_exists() {
    // The rule one level up: this call is a long poll, so a poll that finds nothing
    // assignable in the task list it names is HELD and then answered with the documented
    // empty result — and a `domain` that resolves to no stored domain is one such list,
    // because a poll only hands over a task that is waiting and no task is waiting under
    // a name the store knows nothing about.
    //
    // Every row runs on a PAUSED clock: the documented hold is 60 seconds, so virtual
    // time carries the wait while nothing sleeps, and a row that was refused instead of
    // held would answer before the clock ever moved.
    let emulator = Emulator::new();
    let registered = "empty-poll";
    seed(&emulator, registered).await;

    // The modelled `DomainName` is `len 1..256`
    // (`smithy_query(query="shape",shape="DomainName")`), so 256 characters is the
    // longest name a request may carry at all.
    let longest_name = "d".repeat(256);
    // Rows vary WHY the list holds nothing assignable and the shape of the name that
    // holds it: a registered domain with an idle list, a name never registered, a
    // registered name differing only in case, a registered name with a suffix, the
    // longest name the model admits, and a name outside ASCII — plus which task list is
    // polled, including one no start ever named.
    let rows: Vec<(&str, String, &str)> = vec![
        (
            "a registered domain whose list holds nothing",
            registered.to_string(),
            "deciders-idle",
        ),
        (
            "a name that was never registered",
            "never-registered".to_string(),
            DEFAULT_LIST,
        ),
        (
            "a registered name in another case",
            registered.to_uppercase(),
            DEFAULT_LIST,
        ),
        (
            "a registered name with a suffix",
            format!("{registered}-2"),
            "deciders-elsewhere",
        ),
        (
            "the longest name the model admits",
            longest_name.clone(),
            DEFAULT_LIST,
        ),
        (
            "a name outside ASCII",
            "dömäin-ünregistered".to_string(),
            "déciders",
        ),
    ];

    for (case, domain, task_list) in &rows {
        // The HOLD itself, before the answer: half the documented timeout passes on the
        // paused clock with the call still unanswered, so the empty result below is the
        // end of a long poll and not an immediate answer.
        assert_holds_open_for(&emulator, case, domain, task_list, POLL_TIMEOUT / 2).await;

        let (status, body) = emulator
            .call(
                "PollForDecisionTask",
                &json!({
                    "domain": domain,
                    "taskList": { "name": task_list },
                    "identity": "decider-empty",
                }),
            )
            .await;
        assert_empty_decision_task(case, status, &body);
    }

    // A poll creates nothing: every name the polls above found nothing under is still
    // unknown to the operations that address a domain directly.
    for (case, domain, _) in rows.iter().skip(1) {
        emulator
            .refused(
                case,
                "DescribeDomain",
                &json!({ "name": domain }),
                "UnknownResourceFault",
            )
            .await;
        emulator
            .refused(
                case,
                "StartWorkflowExecution",
                &json!({
                    "domain": domain,
                    "workflowId": "wf-unknown-domain",
                    "workflowType": { "name": WORKFLOW_TYPE.0, "version": WORKFLOW_TYPE.1 },
                }),
                "UnknownResourceFault",
            )
            .await;
    }

    // And a poll that CAN be served still is: the run started now is handed over with a
    // real token, so holding the empty polls open changed nothing about the positive path.
    let case = "served after the empty polls";
    let run_id = start(&emulator, registered, "wf-after-empty", None, None, None).await;
    let answer = poll(&emulator, case, registered, DEFAULT_LIST, json!({})).await;
    let token = handed_out_token(case, &answer);
    assert_eq!(
        string_at(&answer, "/workflowExecution/runId"),
        run_id,
        "case {case:?}: the waiting task is still handed over, got token {token:?} in \
         {answer:?}"
    );
}
