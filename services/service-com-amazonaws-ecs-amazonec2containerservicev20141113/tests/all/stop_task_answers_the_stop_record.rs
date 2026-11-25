// SPDX-License-Identifier: CC0-1.0

//! What `StopTask` ANSWERS for a task that has already stopped — driven end to end over the
//! shared wire harness in [`crate::ecs_wire`], so every value asserted here crossed the wire
//! of the operation that produced it.
//!
//! # The rule under test
//!
//! > `StopTask` reports "the task that was stopped", and what it does is issued "to the
//! > containers running in the task" — so its output is an account of a STOP. A task that has
//! > already reached its terminal `STOPPED` holds nothing for the call to act on: its
//! > containers have exited and the resources Amazon ECS allocated for it are released, so
//! > the record it answers is the stop record alone — the `lastStatus`/`desiredStatus` it
//! > settled at, the `stopCode` and `stoppedReason` it stopped WITH and the instants it
//! > stopped at, beside where and what it ran — with no container, no attachment and no
//! > per-container override on it, and nothing of that record overwritten by the later
//! > caller's own reason. What such a task HELD is read through the describe path, which goes
//! > on reporting every container it ran and every attachment it was allocated in the state
//! > the task left them in.
//!
//! A task that is still on its way out is the other side of the same rule: it has not reached
//! its terminal stop, so the stop it reports is the one in progress and its containers are
//! still on the record.
//!
//! # What the rows span
//!
//! Whatever the rule generalises over: HOW the task reached `STOPPED` (its essential
//! container's program ended, or a caller asked for the stop) and so which `TaskStopCode` and
//! which `stoppedReason` the record carries, the network mode it ran under — and so whether
//! it was allocated an elastic network interface of its own at all — the launch selection
//! (the capacity registered to the cluster, Fargate), and how many containers its revision
//! declares, which is how many per-container overrides the task runs under.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    cluster_with_capacity, list, ok, poll_until, service, string, AWSVPC_SUBNET,
};

/// The cluster every placement below is made in.
const CLUSTER: &str = "stoprecord";

/// The image every container names. Nothing is fetched from it here: what a container runs is
/// the `command` its definition states.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// A program that keeps its container up, so the task it belongs to reaches `RUNNING` and
/// stays there until something stops it.
const STAYS_UP: [&str; 3] = ["sh", "-c", "while true; do sleep 1; done"];

/// `Task$stopCode` of a task whose essential container exited
/// (`smithy_query(query="shape",shape="TaskStopCode")` -> EssentialContainerExited,
/// InfrastructureHealth, ServiceSchedulerInitiated, SpotInterruption, TaskFailedToStart,
/// TerminationNotice, UserInitiated).
const ESSENTIAL_CONTAINER_EXITED: &str = "EssentialContainerExited";

/// The same enum's member for a stop a caller asked for.
const USER_INITIATED: &str = "UserInitiated";

/// The reason the FIRST stop of a caller-stopped row names, which is the reason that row's
/// stop record must keep.
const FIRST_REASON: &str = "the stop that happened";

/// The reason a LATER `StopTask` of an already stopped task names, which must reach no
/// record: the task stopped for the reason it stopped for.
const LATER_REASON: &str = "asked again, long afterwards";

/// How a row's task reaches its terminal `STOPPED`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stops {
    /// Its essential container's program ends, which stops the task with no call at all.
    EssentialContainerExits,
    /// A caller asks for the stop, naming [`FIRST_REASON`].
    CallerAsks,
}

/// One row: a revision to place, how its task stops, and what its stop record must carry.
#[derive(Debug, Clone)]
struct Row {
    case: &'static str,
    /// The family this row registers, so each row reads its own revision back.
    family: &'static str,
    /// The containers the revision declares, in order — one per-container override each.
    containers: Vec<Value>,
    /// The `networkMode` the revision declares, and `None` for a revision that declares
    /// none, whose task shares the networking of the host that runs it.
    network_mode: Option<&'static str>,
    /// What the placement names: a `launchType`, with the `awsvpc` placement carrying the
    /// one subnet the harness holds.
    launch_type: &'static str,
    /// Whether the task is allocated an elastic network interface of its own, which is what
    /// the `awsvpc` network mode gets it: the attachment the describe path reports.
    holds_interface: bool,
    /// How the task reaches `STOPPED`.
    stops: Stops,
    /// The `stopCode` the terminal record must carry.
    stop_code: &'static str,
}

/// One container definition running `command`.
fn container(name: &str, command: &[&str]) -> Value {
    json!({
        "name": name,
        "image": IMAGE,
        "memory": 128,
        "cpu": 0,
        "essential": true,
        "command": command,
    })
}

/// The rows, spanning everything the rule generalises over.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one container, its program ends, on the cluster's own capacity",
            family: "stoprecordexit",
            containers: vec![container("app", &["sh", "-c", "echo leaving; exit 7"])],
            network_mode: None,
            launch_type: "EC2",
            holds_interface: false,
            stops: Stops::EssentialContainerExits,
            stop_code: ESSENTIAL_CONTAINER_EXITED,
        },
        Row {
            case: "one container, a caller asks for the stop, bridge networking",
            family: "stoprecordbridge",
            containers: vec![container("app", &STAYS_UP)],
            network_mode: Some("bridge"),
            launch_type: "EC2",
            holds_interface: false,
            stops: Stops::CallerAsks,
            stop_code: USER_INITIATED,
        },
        Row {
            case: "two containers, a caller asks for the stop, an awsvpc Fargate task",
            family: "stoprecordawsvpc",
            containers: vec![container("app", &STAYS_UP), container("sidecar", &STAYS_UP)],
            network_mode: Some("awsvpc"),
            launch_type: "FARGATE",
            holds_interface: true,
            stops: Stops::CallerAsks,
            stop_code: USER_INITIATED,
        },
    ]
}

#[tokio::test]
async fn a_stopped_task_s_stop_answers_its_stop_record_alone() {
    let router = service();
    cluster_with_capacity(&router, CLUSTER, "setup").await;

    for row in rows() {
        let case = row.case;
        register(&router, &row, case).await;
        let task_arn = place(&router, &row, case).await;

        // Wait for the CONDITION each row is about — the task reaching its terminal stop —
        // re-reading it until it holds.
        let stopped = match row.stops {
            Stops::EssentialContainerExits => {
                poll_until(
                    case,
                    "the task to stop when its essential container exited",
                    || describe(&router, &task_arn, case),
                    |task| task["lastStatus"] == json!("STOPPED"),
                )
                .await
            }
            Stops::CallerAsks => {
                // The task has to be up before a caller can stop a running task.
                poll_until(
                    case,
                    "the task to start",
                    || describe(&router, &task_arn, case),
                    |task| task["lastStatus"] == json!("RUNNING"),
                )
                .await;
                // The OTHER side of the rule: the stop this call starts is not terminal yet,
                // so the task it answers is still the live record — its containers on it.
                let asked = stop(&router, &task_arn, FIRST_REASON, case).await;
                assert_eq!(
                    (
                        asked["lastStatus"].as_str(),
                        asked["desiredStatus"].as_str(),
                        list(&asked, "containers", case).len(),
                    ),
                    (
                        Some("DEACTIVATING"),
                        Some("STOPPED"),
                        row.containers.len()
                    ),
                    "case {case:?}: expected a task still on its way out to carry its \
                     containers, got {asked}"
                );
                poll_until(
                    case,
                    "the task to reach its terminal stop",
                    || describe(&router, &task_arn, case),
                    |task| task["lastStatus"] == json!("STOPPED"),
                )
                .await
            }
        };

        // What the task HELD is still read through the describe path: every container it ran,
        // and the attachment of the interface it was allocated, in the state it left them.
        assert_eq!(
            list(&stopped, "containers", case).len(),
            row.containers.len(),
            "case {case:?}: expected the stopped task to keep reporting its containers, got \
             {stopped}"
        );
        assert_eq!(
            list(&stopped, "attachments", case).len(),
            usize::from(row.holds_interface),
            "case {case:?}: expected the describe path to report the interface the task \
             held, got {stopped}"
        );
        assert_eq!(
            list(&stopped["overrides"], "containerOverrides", case).len(),
            row.containers.len(),
            "case {case:?}: expected the overrides the task ran under to name each of its \
             containers, got {stopped}"
        );

        // And the stop of a task that has ALREADY stopped: the stop record alone.
        let answered = stop(&router, &task_arn, LATER_REASON, case).await;
        assert_eq!(
            (
                answered["lastStatus"].as_str(),
                answered["desiredStatus"].as_str(),
                answered["stopCode"].as_str(),
            ),
            (Some("STOPPED"), Some("STOPPED"), Some(row.stop_code)),
            "case {case:?}: expected the terminal stop record it found, got {answered}"
        );
        let reason = string(&answered, "stoppedReason", case);
        assert_ne!(
            reason, LATER_REASON,
            "case {case:?}: a later caller's reason must not overwrite the stop that \
             happened, got {answered}"
        );
        assert_eq!(
            reason,
            string(&stopped, "stoppedReason", case),
            "case {case:?}: expected the stop record the task already carried, got {answered}"
        );
        for member in ["stoppedAt", "executionStoppedAt", "stoppingAt"] {
            assert!(
                !answered[member].is_null(),
                "case {case:?}: expected {member} on the stop record, got {answered}"
            );
        }
        // Where and what it ran is part of that record, so the stop does not forget it.
        assert_eq!(
            (
                answered["taskArn"].as_str(),
                answered["clusterArn"].is_null(),
                answered["launchType"].as_str(),
            ),
            (Some(task_arn.as_str()), false, Some(row.launch_type)),
            "case {case:?}: expected the placement record beside the stop, got {answered}"
        );
        // Nothing of the task is still LIVE, so none of it is reported: no container to
        // signal, no attachment to tear down, and no per-container override to apply.
        for member in ["containers", "attachments"] {
            assert!(
                list(&answered, member, case).is_empty(),
                "case {case:?}: expected no {member} on the stop record, got {answered}"
            );
        }
        assert!(
            list(&answered["overrides"], "containerOverrides", case).is_empty(),
            "case {case:?}: expected no container overrides on the stop record, got {answered}"
        );
    }
}

/// Register the row's revision.
async fn register(router: &MultiServiceRouter, row: &Row, case: &str) {
    let mut request = json!({
        "family": row.family,
        "containerDefinitions": row.containers,
    });
    if let Some(mode) = row.network_mode {
        request["networkMode"] = json!(mode);
    }
    if row.launch_type == "FARGATE" {
        request["requiresCompatibilities"] = json!(["FARGATE"]);
        request["cpu"] = json!("256");
        request["memory"] = json!("512");
    }
    ok(router, "RegisterTaskDefinition", request, case).await;
}

/// Place one task of the row's revision and answer its ARN.
async fn place(router: &MultiServiceRouter, row: &Row, case: &str) -> String {
    let mut request = json!({
        "cluster": CLUSTER,
        "taskDefinition": row.family,
        "launchType": row.launch_type,
    });
    if row.network_mode == Some("awsvpc") {
        request["networkConfiguration"] = json!({
            "awsvpcConfiguration": {
                "subnets": [AWSVPC_SUBNET],
                "assignPublicIp": "ENABLED",
            },
        });
    }
    let placed = ok(router, "RunTask", request, case).await;
    string(&list(&placed, "tasks", case)[0], "taskArn", case)
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
        .unwrap_or_else(|| panic!("case {case:?}: expected the task {task_arn} back, got {body}"))
}

/// `StopTask` on one task, naming `reason`, and the task it answered.
async fn stop(router: &MultiServiceRouter, task_arn: &str, reason: &str, case: &str) -> Value {
    let body = ok(
        router,
        "StopTask",
        json!({ "cluster": CLUSTER, "task": task_arn, "reason": reason }),
        case,
    )
    .await;
    body["task"].clone()
}
