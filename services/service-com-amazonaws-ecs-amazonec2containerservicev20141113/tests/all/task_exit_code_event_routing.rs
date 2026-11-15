// SPDX-License-Identifier: CC0-1.0

//! Routing an `ECS Task State Change` event by the status its CONTAINER exited with.
//!
//! The RULE under test: an `ECS Task State Change` event's `detail` is the task Amazon ECS
//! holds at the instant it moved — the same projection `DescribeTasks` answers with, so
//! `detail.containers[].exitCode`, `detail.stopCode` and `detail.stoppedReason` are the
//! task's own, and `detail.version` is the counter a task bumps on every change, so the
//! event of a later move carries a higher one than the event of an earlier move. A
//! container that has not ended has produced no exit status, so the event of a task that is
//! running carries none. EventBridge then routes each envelope by its `EventPattern`
//! ALONE: a pattern that constrains a member nested inside the `containers` array selects
//! the tasks whose containers hold that value and withholds the rest, so two rules
//! differing only in the exit status they name split the stopped tasks between their
//! targets and neither sees the other's.
//!
//! The rows span what the rule generalises over: the exit status itself — the success
//! status, the two failure statuses a shell's own utilities and a command's `exit` produce,
//! and the largest status a shell reports (`exit` takes its operand modulo 256, so
//! `0..=255` are the statuses a container can end with). Each row's status is named by its
//! OWN rule, pointed at its OWN queue, and every row's task is placed in the one cluster at
//! the same time, so the routing is decided by the events alone.
//!
//! The exit statuses below are produced by REAL programs run through the container data
//! plane, and every value asserted is read back off the delivered envelope or through
//! `DescribeTasks` — nothing here reaches into the state struct.
//!
//! EventBridge and Amazon SQS are mounted BESIDE Amazon ECS on the same engine
//! ([`crate::task_state_change_events::emulator`]), so a publication travels the real
//! in-process callback, through the real bus, to a real queue.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    awsvpc_network_configuration, list, ok, string, ACCOUNT, AWSVPC_START, SIGNED_REGION,
};
use crate::task_state_change_events::{drain, emulator, given_queue, given_rule, grant_delivery};

/// The `source` and `detail-type` Amazon ECS publishes a task's state changes under,
/// spelled out here rather than imported, so a wrong constant in the implementation cannot
/// make a test pass.
const EVENT_SOURCE: &str = "aws.ecs";
const TASK_STATE_CHANGE: &str = "ECS Task State Change";

/// The cluster every task below runs in.
const CLUSTER: &str = "exitrouting";

/// The image every container below starts from. Nothing is fetched from it: what runs is
/// the `command` the revision states.
const IMAGE: &str = "public.ecr.aws/docker/library/busybox:latest";

/// The task statuses this suite reads off the delivered events.
const RUNNING: &str = "RUNNING";
const STOPPED: &str = "STOPPED";

/// How often a poll re-reads the target queues.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long the poll keeps trying before it gives up: the whole start of an `awsvpc` task
/// ([`AWSVPC_START`]), the work a standalone task is placed to do
/// (`state::CONTAINER_WORK_SPAN`), the stop that follows it and the delivery of the events,
/// with generous margin for a loaded test machine.
const POLL_BUDGET: Duration = AWSVPC_START
    .saturating_add(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::state::CONTAINER_WORK_SPAN,
    )
    .saturating_add(Duration::from_secs(30));

/// One row: the status a task's container exits with, and so the status the rule that is to
/// receive that task's `STOPPED` event names.
struct Row {
    case: &'static str,
    /// The status the container's `exit` ends with — a shell reports `0..=255`.
    exit_code: i64,
}

/// The table spans the exit status the routing keys off.
const ROWS: &[Row] = &[
    Row {
        case: "a container that exits zero",
        exit_code: 0,
    },
    Row {
        case: "a container that exits one",
        exit_code: 1,
    },
    Row {
        case: "a container that exits seven",
        exit_code: 7,
    },
    Row {
        // The largest status a shell reports: `exit` takes its operand modulo 256.
        case: "a container that exits the largest status a shell reports",
        exit_code: 255,
    },
];

/// The ARN of the cluster this suite runs its tasks in.
fn cluster_arn() -> String {
    format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:cluster/{CLUSTER}")
}

/// The pattern of a rule scoped to the tasks of one cluster that STOPPED with a container
/// that exited `exit_code`: the member it constrains sits inside the event's `containers`
/// array.
fn exit_code_pattern(cluster_arn: &str, exit_code: i64) -> Value {
    json!({
        "source": [EVENT_SOURCE],
        "detail-type": [TASK_STATE_CHANGE],
        "detail": {
            "clusterArn": [cluster_arn],
            "lastStatus": [STOPPED],
            "containers": { "exitCode": [exit_code] },
        },
    })
}

/// The pattern of a rule that takes EVERY move of the cluster's tasks, whatever it moved
/// into: what the version and exit-status projection is read off.
fn every_move_pattern(cluster_arn: &str) -> Value {
    json!({
        "source": [EVENT_SOURCE],
        "detail-type": [TASK_STATE_CHANGE],
        "detail": { "clusterArn": [cluster_arn] },
    })
}

/// The exit statuses the containers of one event's `detail` report, in order. A container
/// that has produced none carries no `exitCode` member at all, so it contributes nothing.
fn exit_codes_of(event: &Value) -> Vec<i64> {
    event["detail"]["containers"]
        .as_array()
        .map(|containers| {
            containers
                .iter()
                .filter_map(|container| container["exitCode"].as_i64())
                .collect()
        })
        .unwrap_or_default()
}

/// The `detail.version` one event carries; `None` when it carries none.
fn version_of(event: &Value) -> Option<i64> {
    event["detail"]["version"].as_i64()
}

/// The events of one task, in the order they arrived.
fn events_of<'a>(events: &'a [Value], task_arn: &str) -> Vec<&'a Value> {
    events
        .iter()
        .filter(|event| event["detail"]["taskArn"] == json!(task_arn))
        .collect()
}

/// The first collected event reporting `status`.
fn event_in<'a>(events: &[&'a Value], status: &str) -> Option<&'a Value> {
    events
        .iter()
        .copied()
        .find(|event| event["detail"]["lastStatus"] == json!(status))
}

/// Register one FARGATE revision whose single, essential container exits with `exit_code`,
/// and place one task of it. Answers the task's ARN.
async fn place_task(
    router: &MultiServiceRouter,
    family: &str,
    exit_code: i64,
    case: &str,
) -> String {
    ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            // The documented Fargate task size the recorded scenario uses.
            "cpu": "256",
            "memory": "512",
            "containerDefinitions": [{
                "name": "app",
                "image": IMAGE,
                "essential": true,
                "command": ["sh", "-c", format!("exit {exit_code}")],
            }],
        }),
        case,
    )
    .await;
    let placed = ok(
        router,
        "RunTask",
        json!({
            "cluster": CLUSTER,
            "taskDefinition": family,
            "launchType": "FARGATE",
            "count": 1,
            "networkConfiguration": awsvpc_network_configuration(),
        }),
        case,
    )
    .await;
    let tasks = list(&placed, "tasks", case);
    assert_eq!(
        tasks.len(),
        1,
        "case {case:?}: expected RunTask to place one task, got {placed}"
    );
    string(&tasks[0], "taskArn", case)
}

/// One task, as `DescribeTasks` reports it.
async fn describe(router: &MultiServiceRouter, task_arn: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": [task_arn] }),
        case,
    )
    .await;
    list(&body, "tasks", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected one task back, got {body}"))
}

#[tokio::test]
async fn a_stopped_task_event_reaches_only_the_rule_naming_its_container_exit_status() {
    let setup = "rules differing only in the exit status they name";
    let router = emulator();
    let cluster_arn = cluster_arn();
    ok(
        &router,
        "CreateCluster",
        json!({ "clusterName": CLUSTER }),
        setup,
    )
    .await;

    // One rule per row, each pointed at its own queue, and one rule taking every move of
    // the cluster's tasks.
    let mut scoped: Vec<String> = Vec::new();
    for row in ROWS {
        let name = format!("exit-{}", row.exit_code);
        let (url, queue_arn) = given_queue(&router, row.case, &name).await;
        grant_delivery(&router, row.case, &url, &queue_arn).await;
        given_rule(
            &router,
            row.case,
            &format!("stopped-with-{name}"),
            exit_code_pattern(&cluster_arn, row.exit_code),
            &queue_arn,
        )
        .await;
        scoped.push(url);
    }
    let (every_move_url, every_move_arn) = given_queue(&router, setup, "every-move").await;
    grant_delivery(&router, setup, &every_move_url, &every_move_arn).await;
    given_rule(
        &router,
        setup,
        "every-task-move",
        every_move_pattern(&cluster_arn),
        &every_move_arn,
    )
    .await;

    // Every row's task is placed before any of them is waited on, so all of them run at
    // once and the routing is decided by the events alone.
    let mut task_arns: Vec<String> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let family = format!("exitrouting{index}");
        task_arns.push(place_task(&router, &family, row.exit_code, row.case).await);
    }

    // ONLY the queues are polled from here: a client that watches a task through
    // EventBridge makes no further Amazon ECS call. The loop waits for the CONDITION — each
    // scoped queue holding its own task's stop, and every task's stop on the queue that
    // takes every move — and stops the instant it holds.
    let mut delivered: Vec<Vec<Value>> = ROWS.iter().map(|_| Vec::new()).collect();
    let mut moves: Vec<Value> = Vec::new();
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        for (index, url) in scoped.iter().enumerate() {
            let batch = drain(&router, ROWS[index].case, url).await;
            delivered[index].extend(batch);
        }
        moves.extend(drain(&router, setup, &every_move_url).await);
        let settled = delivered.iter().all(|events| !events.is_empty())
            && task_arns.iter().all(|task_arn| {
                event_in(&events_of(&moves, task_arn), STOPPED).is_some()
                    && event_in(&events_of(&moves, task_arn), RUNNING).is_some()
            });
        if settled || Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let task_arn = &task_arns[index];
        let wanted = row.exit_code;
        let scoped_events = &delivered[index];
        assert!(
            !scoped_events.is_empty(),
            "case {case:?}: expected the rule naming exit status {wanted} to receive the \
             STOPPED event of task {task_arn}, got nothing"
        );
        // Every envelope this rule's target received is an ECS task state change about THIS
        // row's task, stopped, carrying THIS row's exit status: the sibling rules' tasks are
        // withheld from it.
        for event in scoped_events {
            assert_eq!(
                event["source"],
                json!(EVENT_SOURCE),
                "case {case:?}: expected source {EVENT_SOURCE}, got {event}"
            );
            assert_eq!(
                event["detail-type"],
                json!(TASK_STATE_CHANGE),
                "case {case:?}: expected detail-type {TASK_STATE_CHANGE}, got {event}"
            );
            assert_eq!(
                event["detail"]["taskArn"],
                json!(task_arn),
                "case {case:?}: a rule naming exit status {wanted} must receive only the \
                 task that exited it ({task_arn}), got {event}"
            );
            assert_eq!(
                event["detail"]["lastStatus"],
                json!(STOPPED),
                "case {case:?}: expected a STOPPED event, got {event}"
            );
            assert_eq!(
                exit_codes_of(event),
                vec![wanted],
                "case {case:?}: expected the event's containers to report exit status \
                 {wanted}, got {event}"
            );
        }

        // What the event says about the stop is what the control plane says about it.
        let task = describe(&router, task_arn, case).await;
        let reported: Vec<i64> = list(&task, "containers", case)
            .iter()
            .filter_map(|container| container["exitCode"].as_i64())
            .collect();
        let last = scoped_events
            .last()
            .unwrap_or_else(|| panic!("case {case:?}: expected a delivered STOPPED event"));
        assert_eq!(
            exit_codes_of(last),
            reported,
            "case {case:?}: the event's container exit statuses must be the task's, got \
             {last} and {task}"
        );
        assert_eq!(
            last["detail"]["stopCode"],
            task["stopCode"].clone(),
            "case {case:?}: the event's stopCode must be the task's, got {last} and {task}"
        );
        assert_eq!(
            last["detail"]["stoppedReason"],
            task["stoppedReason"].clone(),
            "case {case:?}: the event's stoppedReason must be the task's, got {last} and \
             {task}"
        );

        // The other rules' targets never saw this task.
        for (other, url) in scoped.iter().enumerate() {
            if other == index {
                continue;
            }
            let others = events_of(&delivered[other], task_arn);
            assert!(
                others.is_empty(),
                "case {case:?}: the rule naming exit status {} must not receive the task \
                 that exited {wanted} ({url}), got {others:?}",
                ROWS[other].exit_code
            );
        }

        // The run of this task, as the rule that takes every move saw it: the RUNNING event
        // carries a LOWER version than the STOPPED one — the counter the task bumps on
        // every change — and reports no exit status, because at that instant nothing of the
        // container had ended.
        let all = events_of(&moves, task_arn);
        let running = event_in(&all, RUNNING).unwrap_or_else(|| {
            panic!("case {case:?}: expected a RUNNING event for {task_arn}, got {all:?}")
        });
        let stopped = event_in(&all, STOPPED).unwrap_or_else(|| {
            panic!("case {case:?}: expected a STOPPED event for {task_arn}, got {all:?}")
        });
        let (running_version, stopped_version) = (version_of(running), version_of(stopped));
        assert!(
            running_version.is_some_and(|earlier| {
                stopped_version.is_some_and(|later| earlier < later)
            }),
            "case {case:?}: expected the RUNNING event's version below the STOPPED event's, \
             got {running_version:?} and {stopped_version:?} ({running} then {stopped})"
        );
        assert!(
            exit_codes_of(running).is_empty(),
            "case {case:?}: a task that is RUNNING has produced no exit status, got {running}"
        );
        assert_eq!(
            exit_codes_of(stopped),
            vec![wanted],
            "case {case:?}: expected the STOPPED event to report exit status {wanted}, got \
             {stopped}"
        );
    }
}
