// SPDX-License-Identifier: CC0-1.0

//! The ONE refusal an `ExecuteCommand` earns when the container it names runs no
//! execute-command agent, whatever left it without one.
//!
//! # The rule under test
//!
//! > A session is opened on the `ExecuteCommandAgent` managed agent inside the container, so
//! > a call whose target container has no RUNNING one is refused HTTP 400
//! > `InvalidParameterException` with the one fixed sentence Amazon ECS states both of those
//! > conditions in — "execute command was not enabled when the task was run or the execute
//! > command agent isn't running" — and that class and that sentence do not change with the
//! > REASON the container has no running agent. `TargetNotConnectedException` is not the
//! > answer for it: the model reserves that class for the configuration issues it documents
//! > ("Incorrect IAM permissions", "The SSM agent is not installed or is not running", "There
//! > is an interface Amazon VPC endpoint for Amazon ECS, but there is not one for Systems
//! > Manager Session Manager"), so a client branching on the error code never sees it where
//! > the remedy is to wait or to relaunch with the feature on.
//!
//! # What the rows span
//!
//! One row per REASON a container is left without a running agent, which is what the rule
//! generalises over: the launch never turned the feature on; the feature is on but the task
//! role authorizes only part of the Systems Manager channel the agent connects over; the role
//! allows the whole channel with one action denied beside the allow; and the agent connected
//! and then ended with its container when the task was stopped. A fifth row is the control —
//! the feature on and the channel authorized — which is answered with a session, so the
//! refusal of the other four is read as the agent's absence rather than as a blanket refusal.
//! Every row is otherwise identical: the same cluster, the same revision shape, the same
//! Fargate `awsvpc` placement, and a task role IAM really holds.
//!
//! Each row is judged twice over: on the agent `DescribeTasks` reports for its container
//! (absent, `PENDING`, `STOPPED` or `RUNNING`) and on what `ExecuteCommand` answers, and the
//! call is made in two request forms — the task by ARN with the container named, and the task
//! by id with none — so the outcome is asserted to turn on the agent and not on how the
//! request spelled its target.
//!
//! Every value is read back through the ECS front door, and every role and policy is created
//! through IAM's OWN API rather than written into its store.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    awsvpc_network_configuration, create_cluster, emulator_with_ecr_and_iam, iam, list, ok,
    poll_until, refusal, string, ACCOUNT,
};

/// The cluster every row places into.
const CLUSTER: &str = "execagentabsence";

/// The container every revision of this suite declares, and the image it runs.
const CONTAINER: &str = "app";
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// The managed agent the execute command feature runs inside a container
/// (`smithy_query(query="shape",shape="ManagedAgentName")` -> `ExecuteCommandAgent`).
const EXECUTE_COMMAND_AGENT: &str = "ExecuteCommandAgent";

/// The `ManagedAgent$lastStatus` values this suite reads back: an agent that has connected, one
/// that has not, and one that ended with its container.
const RUNNING: &str = "RUNNING";
const PENDING: &str = "PENDING";
const STOPPED: &str = "STOPPED";

/// The four actions the execute-command agent opens its Systems Manager channels with, which
/// the task role has to allow for the agent to connect at all (Amazon ECS Exec's documented
/// task-role requirement, as `state::SSM_MESSAGES_CHANNEL_ACTIONS` holds them).
const CHANNEL_ACTIONS: [&str; 4] = [
    "ssmmessages:CreateControlChannel",
    "ssmmessages:CreateDataChannel",
    "ssmmessages:OpenControlChannel",
    "ssmmessages:OpenDataChannel",
];

/// Only the two channels the agent CREATES, which is half of what it needs.
const ONLY_THE_CREATES: [&str; 2] = [
    "ssmmessages:CreateControlChannel",
    "ssmmessages:CreateDataChannel",
];

/// The service principal a task role is assumed by, and the action its trust policy allows.
const ECS_TASKS_PRINCIPAL: &str = "ecs-tasks.amazonaws.com";
const ASSUME_ROLE: &str = "sts:AssumeRole";

/// `InvalidParameterException$message` for a call whose target container runs no
/// execute-command agent: fixed text, no part of it taken from the request or from the store,
/// stating both conditions that leave a container without one.
const NOT_AVAILABLE: &str =
    "The execute command failed because execute command was not enabled when the task was \
     run or the execute command agent isn't running. Wait and try again or run a new task \
     with execute command enabled and try again.";

/// The error code every row of the class earns, and the one the model reserves for the
/// configuration issues it documents — which is NOT this class.
const INVALID_PARAMETER: &str = "InvalidParameterException";
const TARGET_NOT_CONNECTED: &str = "TargetNotConnectedException";

/// What one row's task role states about the agent's Systems Manager channel.
#[derive(Debug, Clone, Copy)]
enum Grant {
    /// An `Allow` over all four channel actions.
    WholeChannel,
    /// An `Allow` over the actions named, which is less than the agent needs.
    Only(&'static [&'static str]),
    /// An `Allow` over all four, beside a `Deny` of the action named.
    WholeChannelButDeny(&'static str),
}

/// One row: what left (or did not leave) its container without a running agent.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    /// Whether the LAUNCH turned the execute command feature on.
    enabled: bool,
    /// What the task role it runs under states about the agent's channel.
    grant: Grant,
    /// Whether the row stops its task — after waiting for the agent to connect — before
    /// calling `ExecuteCommand`.
    stopped: bool,
    /// The `lastStatus` `DescribeTasks` must report for the container's execute-command
    /// agent, and `None` for a container that carries no such agent at all.
    agent: Option<&'static str>,
    /// Whether the call is refused, or answered with a session.
    refused: bool,
}

const ROWS: [Row; 5] = [
    Row {
        case: "a task the launch did not turn the feature on for",
        enabled: false,
        grant: Grant::WholeChannel,
        stopped: false,
        agent: None,
        refused: true,
    },
    Row {
        case: "a task role allowing only the channels the agent creates",
        enabled: true,
        grant: Grant::Only(&ONLY_THE_CREATES),
        stopped: false,
        agent: Some(PENDING),
        refused: true,
    },
    Row {
        case: "a task role denying one channel action beside the allow",
        enabled: true,
        grant: Grant::WholeChannelButDeny("ssmmessages:OpenDataChannel"),
        stopped: false,
        agent: Some(PENDING),
        refused: true,
    },
    Row {
        case: "a task stopped after its agent had connected",
        enabled: true,
        grant: Grant::WholeChannel,
        stopped: true,
        agent: Some(STOPPED),
        refused: true,
    },
    Row {
        case: "a running task whose role authorizes the whole channel",
        enabled: true,
        grant: Grant::WholeChannel,
        stopped: false,
        agent: Some(RUNNING),
        refused: false,
    },
];

/// An identity-based policy document with one statement per entry: the effect over the
/// actions, for every resource — the channel actions carry no resource of their own.
fn policy(statements: &[(&str, &[&str])]) -> String {
    let statements: Vec<Value> = statements
        .iter()
        .map(|(effect, actions)| json!({ "Effect": effect, "Action": actions, "Resource": "*" }))
        .collect();
    json!({ "Version": "2012-10-17", "Statement": statements }).to_string()
}

/// The task role one row runs under, created through IAM's own API: trusted by the task
/// service principal — an exec-enabled launch is refused a role Amazon ECS cannot assume —
/// and carrying the row's own statement about the agent's channel.
async fn task_role(router: &MultiServiceRouter, index: usize, row: &Row) -> String {
    let name = format!("execAbsenceRole{index}");
    let document = match row.grant {
        Grant::WholeChannel => policy(&[("Allow", &CHANNEL_ACTIONS)]),
        Grant::Only(actions) => policy(&[("Allow", actions)]),
        Grant::WholeChannelButDeny(denied) => {
            policy(&[("Allow", &CHANNEL_ACTIONS), ("Deny", &[denied])])
        }
    };
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": [ECS_TASKS_PRINCIPAL] },
            "Action": [ASSUME_ROLE],
        }],
    })
    .to_string();
    iam(
        router,
        "CreateRole",
        &[("RoleName", name.clone()), ("AssumeRolePolicyDocument", trust)],
        row.case,
    )
    .await;
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", name.clone()),
            ("PolicyName", "execAbsenceChannel".to_string()),
            ("PolicyDocument", document),
        ],
        row.case,
    )
    .await;
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// Register one Fargate/`awsvpc` revision of `family` running under `role_arn`, with a
/// container whose program outlives this suite: a standalone task is stopped once its
/// essential containers have exited, so a program still being performed is what keeps a row's
/// task RUNNING while the call is made against it.
async fn register(router: &MultiServiceRouter, family: &str, role_arn: &str, case: &str) {
    ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "networkMode": "awsvpc",
            "cpu": "256",
            "memory": "512",
            "requiresCompatibilities": ["FARGATE"],
            "taskRoleArn": role_arn,
            "containerDefinitions": [{
                "name": CONTAINER,
                "image": IMAGE,
                "essential": true,
                "command": ["sleep", "600"],
            }],
        }),
        case,
    )
    .await;
}

/// Place ONE task of `family` with the row's `enableExecuteCommand`, and answer its ARN.
async fn place(router: &MultiServiceRouter, family: &str, enabled: bool, case: &str) -> String {
    let body = ok(
        router,
        "RunTask",
        json!({
            "cluster": CLUSTER,
            "taskDefinition": family,
            "launchType": "FARGATE",
            "count": 1,
            "enableExecuteCommand": enabled,
            "networkConfiguration": awsvpc_network_configuration(),
        }),
        case,
    )
    .await;
    let tasks = list(&body, "tasks", case);
    assert_eq!(
        tasks.len(),
        1,
        "case {case:?}: expected one placed task, got {body}"
    );
    string(&tasks[0], "taskArn", case)
}

/// One task as `DescribeTasks` reports it — the stored record, not the answer of the call that
/// placed it.
async fn describe_task(router: &MultiServiceRouter, task_arn: &str, case: &str) -> Value {
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
        .unwrap_or_else(|| panic!("case {case:?}: expected the task back, got {body}"))
}

/// The `lastStatus` the execute-command agent of the task's container reports, and `None` when
/// that container carries no such agent.
fn agent_status(task: &Value, case: &str) -> Option<String> {
    let container = list(task, "containers", case)
        .into_iter()
        .find(|container| container["name"].as_str() == Some(CONTAINER))?;
    list(&container, "managedAgents", case)
        .into_iter()
        .find(|agent| agent["name"].as_str() == Some(EXECUTE_COMMAND_AGENT))
        .map(|agent| agent["lastStatus"].as_str().unwrap_or_default().to_string())
}

/// The two forms a row's request is made in: the task by ARN with its container named, and the
/// task by id with no container named. "A container name only needs to be specified for tasks
/// containing multiple containers", so both name the same single container of the revision.
fn request_forms(task_arn: &str) -> [(&'static str, Value); 2] {
    let task_id = task_arn
        .rsplit('/')
        .next()
        .expect("a task ARN ends in its task id")
        .to_string();
    [
        (
            "the task by ARN, the container named",
            json!({
                "cluster": CLUSTER,
                "task": task_arn,
                "container": CONTAINER,
                "command": "/bin/sh",
                "interactive": true,
            }),
        ),
        (
            "the task by id, no container named",
            json!({
                "cluster": CLUSTER,
                "task": task_id,
                "command": "/bin/sh",
                "interactive": true,
            }),
        ),
    ]
}

#[tokio::test]
async fn a_container_with_no_running_exec_agent_earns_one_refusal_whatever_left_it_without_one() {
    let router = emulator_with_ecr_and_iam();
    create_cluster(&router, CLUSTER, "the suite's cluster").await;

    // Every row's task is placed first, so the whole table's starts and agent decisions fall
    // due together rather than one wait after another.
    let mut placed: Vec<(&Row, String)> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let role_arn = task_role(&router, index, row).await;
        let family = format!("execabsence{index}");
        register(&router, &family, &role_arn, row.case).await;
        let task_arn = place(&router, &family, row.enabled, row.case).await;
        placed.push((row, task_arn));
    }

    for (row, task_arn) in &placed {
        let case = row.case;
        poll_until(
            case,
            "the task to reach RUNNING",
            || describe_task(&router, task_arn, case),
            |task| task["lastStatus"] == json!(RUNNING),
        )
        .await;

        if row.stopped {
            // The agent runs INSIDE the container, so this row first lets it connect and then
            // takes the container away: what is read back is an agent that HAD connected.
            poll_until(
                case,
                "the agent to connect before the task is stopped",
                || describe_task(&router, task_arn, case),
                |task| agent_status(task, case).as_deref() == Some(RUNNING),
            )
            .await;
            ok(
                &router,
                "StopTask",
                json!({ "cluster": CLUSTER, "task": task_arn, "reason": "the row is done" }),
                case,
            )
            .await;
            poll_until(
                case,
                "the stopped task to reach STOPPED",
                || describe_task(&router, task_arn, case),
                |task| task["lastStatus"] == json!(STOPPED),
            )
            .await;
        }

        // What the row's container reports for its agent. A row whose agent is expected to
        // CONNECT (or to have connected and ended) is waited for, because that is clock-owed;
        // a row whose agent never connects has nothing to wait for, and polling for the
        // status it is expected to stay at reads it the same way.
        let task = match row.agent {
            Some(expected) => {
                poll_until(
                    case,
                    &format!("the agent to report {expected}"),
                    || describe_task(&router, task_arn, case),
                    |task| agent_status(task, case).as_deref() == Some(expected),
                )
                .await
            }
            None => describe_task(&router, task_arn, case).await,
        };
        let reported = agent_status(&task, case);
        assert_eq!(
            reported.as_deref(),
            row.agent,
            "case {case:?}: expected the agent to report {:?}, got {reported:?} in {task}",
            row.agent
        );

        for (form, request) in request_forms(task_arn) {
            let case = format!("{case}, {form}");
            if row.refused {
                // One class and one sentence for the whole table: the reason the container has
                // no running agent changes neither, and the configuration class the model
                // documents separately is not the answer here.
                let (code, message) = refusal(&router, "ExecuteCommand", request, &case).await;
                assert_eq!(
                    (code.as_str(), message.as_str()),
                    (INVALID_PARAMETER, NOT_AVAILABLE),
                    "case {case:?}: expected the agent-not-running refusal, got {code:?} with \
                     {message:?}"
                );
                assert_ne!(
                    code, TARGET_NOT_CONNECTED,
                    "case {case:?}: {TARGET_NOT_CONNECTED} is the class for the configuration \
                     issues the model documents, not for a container with no running agent, \
                     got {message:?}"
                );
                continue;
            }
            // The control: the agent IS running, so a session is opened on it and carried on
            // this emulator's own channel.
            let answer = ok(&router, "ExecuteCommand", request, &case).await;
            let session = &answer["session"];
            let session_id = string(session, "sessionId", &case);
            let stream_url = string(session, "streamUrl", &case);
            assert!(
                stream_url.contains(&session_id)
                    && !string(session, "tokenValue", &case).is_empty(),
                "case {case:?}: expected a session carried on this emulator, got {answer}"
            );
            assert_eq!(
                answer["containerName"].as_str(),
                Some(CONTAINER),
                "case {case:?}: expected the container the session was opened on, got {answer}"
            );
        }
    }
}
