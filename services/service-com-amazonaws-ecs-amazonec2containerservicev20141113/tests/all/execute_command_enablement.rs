// SPDX-License-Identifier: CC0-1.0

//! What `ExecuteCommand` answers once the resources it names resolve, driven end to end
//! over the shared wire harness in [`crate::ecs_wire`].
//!
//! # The rule under test
//!
//! > A session is opened on the `ExecuteCommandAgent` managed agent running inside the
//! > container, and Amazon ECS puts that agent there only because the LAUNCH turned the
//! > feature on ("whether to use the execute command functionality for the containers in
//! > this task"). So the outcome of a call turns on the task's own state: a task launched
//! > WITHOUT `enableExecuteCommand` carries no such agent and earns the ONE fixed
//! > `InvalidParameterException` sentence that states both conditions leaving a task without
//! > one, without saying which of them held; a task launched WITH it reports that agent
//! > running once its containers are running, and the call gets past that condition to the
//! > channel a session's stream is carried over — which this emulator serves no WebSocket
//! > upgrade for, so the target is one no session can be connected to and the call earns the
//! > modelled `TargetNotConnectedException`. A session of any other KIND is not one Amazon
//! > ECS opens at all: `interactive` "only supports initiating interactive sessions, so you
//! > must specify true for this value", which is a property of the request and is refused
//! > whatever task it names.
//!
//! # What the rows span
//!
//! Two tasks differing ONLY in `enableExecuteCommand`, in the same cluster, from the same
//! task-definition family, both advanced to `RUNNING`. The table then varies what the
//! `ExecuteCommand` request carries — the task by id and by full ARN, no container named
//! and each of the revision's two containers named, and both values of `interactive` — so
//! the outcome is asserted to turn on the flag the launch recorded and on the kind of
//! session asked for, and on nothing else about the request. The agent each task does or
//! does not carry is read back through `DescribeTasks`, so the flag is observable in the
//! stored task as well as in the refusal.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{cluster_with_capacity, list, ok, refusal, service, string, ACCOUNT};
use crate::task_lifecycle::container;

/// The cluster both tasks run in.
const CLUSTER: &str = "execflag";

/// The task-definition family both tasks run, with TWO containers so a request that names
/// one names one of several.
const FAMILY: &str = "execflagfamily";

/// The two containers that family declares.
const FIRST_CONTAINER: &str = "app";
const SECOND_CONTAINER: &str = "sidecar";

/// The managed agent the execute command feature runs inside a container
/// (`ManagedAgent$name`: "When the execute command feature is turned on, the managed agent
/// name is `ExecuteCommandAgent`"), which is the agent a session is opened on.
const EXECUTE_COMMAND_AGENT: &str = "ExecuteCommandAgent";

/// `InvalidParameterException$message` for a task no session can be opened on: fixed text,
/// with no part taken from the request, stating both conditions that leave a task without
/// a running execute-command agent.
const NOT_AVAILABLE: &str =
    "The execute command failed because execute command was not enabled when the task was \
     run or the execute command agent isn't running. Wait and try again or run a new task \
     with execute command enabled and try again.";

/// `TargetNotConnectedException$message` for a task whose agent IS running but whose session
/// stream nothing carries: fixed text, with no part taken from the request.
const NOT_CONNECTED: &str = "The execute command failed due to an internal error. Wait and try again.";

/// `InvalidParameterException$message` for a request asking for a session of a kind Amazon
/// ECS does not open, worded as `ExecuteCommandRequest$interactive` states its own rule:
/// "Amazon ECS only supports initiating interactive sessions, so you must specify true for
/// this value."
const INTERACTIVE_ONLY: &str =
    "Amazon ECS only supports initiating interactive sessions, so you must specify true for \
     this value.";

/// How often the fixture re-reads a task it is waiting on, and how long it keeps trying:
/// the start is clock-owed, so the condition is polled for rather than slept on, with
/// generous room for a loaded machine.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const POLL_BUDGET: Duration = Duration::from_secs(60);

/// A container definition of [`FAMILY`]: the harness's minimal definition plus a program
/// that runs for longer than the rows below take.
///
/// A STANDALONE task's work is finished once its essential containers have exited, and
/// Amazon ECS stops the task when it is — so a container whose program is still being
/// performed is what keeps the two tasks of this suite RUNNING while every row is sent
/// against them, rather than any assumption about how fast the host runs.
fn long_running_container(name: &str) -> Value {
    let mut definition = container(name, "public.ecr.aws/nginx/nginx:latest");
    definition["command"] = json!(["sleep", "600"]);
    definition
}

/// A router holding one cluster with capacity and one revision of [`FAMILY`] declaring a
/// task role — an exec-enabled launch needs one ("specify a valid task role in your task
/// definition and try again"), so both tasks are placed from the same revision.
async fn fixture() -> MultiServiceRouter {
    let router = service();
    cluster_with_capacity(&router, CLUSTER, "fixture").await;
    ok(
        &router,
        "RegisterTaskDefinition",
        json!({
            "family": FAMILY,
            "taskRoleArn": format!("arn:aws:iam::{ACCOUNT}:role/execFlagTaskRole"),
            "containerDefinitions": [
                long_running_container(FIRST_CONTAINER),
                long_running_container(SECOND_CONTAINER),
            ],
        }),
        "fixture",
    )
    .await;
    router
}

/// Read ONE task back through `DescribeTasks` — the stored record, not the answer of the
/// call that placed it.
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

/// The `lastStatus` the execute-command agent of `container_name` reports in a described
/// task, and `None` when that container carries no such agent.
fn agent_status(task: &Value, container_name: &str, case: &str) -> Option<String> {
    let container = list(task, "containers", case)
        .into_iter()
        .find(|container| container["name"].as_str() == Some(container_name))
        .unwrap_or_else(|| panic!("case {case:?}: expected a container {container_name}"));
    list(&container, "managedAgents", case)
        .into_iter()
        .find(|agent| agent["name"].as_str() == Some(EXECUTE_COMMAND_AGENT))
        .map(|agent| {
            agent["lastStatus"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
}

/// Run ONE task of [`FAMILY`] with `enable_execute_command`, wait until it is `RUNNING` with
/// its agents settled, and answer its ARN.
async fn running_task(router: &MultiServiceRouter, enabled: bool, case: &str) -> String {
    let body = ok(
        router,
        "RunTask",
        json!({
            "cluster": CLUSTER,
            "taskDefinition": FAMILY,
            "count": 1,
            "enableExecuteCommand": enabled,
        }),
        case,
    )
    .await;
    let tasks = list(&body, "tasks", case);
    assert_eq!(tasks.len(), 1, "case {case:?}: expected one task, got {body}");
    let arn = string(&tasks[0], "taskArn", case);

    // The start is owed by the clock, so the test waits for the CONDITION.
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let task = describe_task(router, &arn, case).await;
        // The flag the placement recorded is the task's own, which is what the gate reads.
        assert_eq!(
            task["enableExecuteCommand"].as_bool().unwrap_or(false),
            enabled,
            "case {case:?}: expected the stored task to report enableExecuteCommand \
             {enabled}, got {task}"
        );
        // The agent Amazon ECS runs inside each container of an exec-enabled task is up
        // once that container is, so the fixture waits for the agent as well as for the
        // task — and a task launched without the flag carries no agent at all, ever.
        let agents_ready = [FIRST_CONTAINER, SECOND_CONTAINER].iter().all(|name| {
            let reported = agent_status(&task, name, case);
            match enabled {
                true => reported.as_deref() == Some("RUNNING"),
                false => reported.is_none(),
            }
        });
        if task["lastStatus"].as_str() == Some("RUNNING") && agents_ready {
            return arn;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the task to reach RUNNING with its agents settled, got \
             {task}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The task id a task ARN ends in.
fn task_id(arn: &str) -> &str {
    arn.rsplit('/')
        .next()
        .expect("a task ARN ends in its task id")
}

#[tokio::test]
async fn execute_command_turns_on_the_flag_the_launch_recorded() {
    let router = fixture().await;
    let disabled = running_task(&router, false, "a task run without execute command").await;
    let enabled = running_task(&router, true, "a task run with execute command").await;

    // Rows: how the request names the task, which container (if any) it names, and whether
    // the session asked for is interactive. Each row is sent against BOTH tasks, which
    // differ only in the flag.
    let rows: [(&str, bool, Option<&str>, bool); 6] = [
        ("the task by id, no container named", false, None, true),
        ("the task by ARN, no container named", true, None, true),
        (
            "the task by id, the first container",
            false,
            Some(FIRST_CONTAINER),
            true,
        ),
        (
            "the task by ARN, the second container",
            true,
            Some(SECOND_CONTAINER),
            true,
        ),
        (
            "a non-interactive session, no container named",
            false,
            None,
            false,
        ),
        (
            "a non-interactive session naming the first container",
            true,
            Some(FIRST_CONTAINER),
            false,
        ),
    ];

    for (row, by_arn, requested, interactive) in rows {
        for (flag, task_arn) in [(false, &disabled), (true, &enabled)] {
            let case = format!("{row}, enableExecuteCommand {flag}");
            let reference = if by_arn {
                task_arn.clone()
            } else {
                task_id(task_arn).to_string()
            };
            let mut request = json!({
                "cluster": CLUSTER,
                "task": reference,
                "command": "/bin/sh",
                "interactive": interactive,
            });
            if let Some(name) = requested {
                request["container"] = json!(name);
            }

            // What each row earns: the kind of session asked for is judged first and is a
            // property of the REQUEST, so it is refused whichever task the request names;
            // after it, the task's own agent decides which of the two modelled conditions
            // the call meets.
            let expected = match (interactive, flag) {
                (false, _) => ("InvalidParameterException", INTERACTIVE_ONLY),
                (true, false) => ("InvalidParameterException", NOT_AVAILABLE),
                (true, true) => ("TargetNotConnectedException", NOT_CONNECTED),
            };
            let (code, message) = refusal(&router, "ExecuteCommand", request, &case).await;
            assert_eq!(
                (code.as_str(), message.as_str()),
                expected,
                "case {case:?}: expected {expected:?}, got {code:?} with {message:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_session_is_attempted_after_the_cluster_task_and_container() {
    let router = fixture().await;
    let enabled = running_task(&router, true, "a task run with execute command").await;

    // The resources the request names are resolved BEFORE the session is attempted, so a
    // request that is wrong about one of them is reported on that resource rather than on
    // the channel the session would be carried over. The rows walk the three resources in
    // the order they are resolved.
    let rows: [(&str, serde_json::Value, &str, &str); 3] = [
        (
            "a cluster the account does not hold",
            json!({ "cluster": "ghostexecflag", "task": enabled.clone() }),
            "ClusterNotFoundException",
            "Cluster not found.",
        ),
        (
            "a task the cluster does not hold",
            json!({ "cluster": CLUSTER, "task": "f".repeat(32) }),
            "InvalidParameterException",
            "The specified task was not found. Specify a valid task ID and try again.",
        ),
        (
            "a container the task does not declare",
            json!({ "cluster": CLUSTER, "task": enabled.clone(), "container": "ghost" }),
            "InvalidParameterException",
            "The specified container was not found. Specify a valid container name and try \
             again.",
        ),
    ];

    for (case, mut request, code, message) in rows {
        request["command"] = json!("/bin/sh");
        request["interactive"] = json!(true);
        let (answered, said) = refusal(&router, "ExecuteCommand", request, case).await;
        assert_eq!(
            (answered.as_str(), said.as_str()),
            (code, message),
            "case {case:?}: expected {code:?} with {message:?}, got {answered:?} with \
             {said:?}"
        );
    }
}

#[tokio::test]
async fn a_stopped_task_has_no_agent_to_open_a_session_on() {
    let router = fixture().await;
    let enabled = running_task(&router, true, "a task run with execute command").await;

    // The agent runs INSIDE the container, so it ends with the container: a task that has
    // been stopped carries no running agent and earns the same refusal a task launched
    // without the flag does — not the connectivity one its running agent earned above. The
    // stop is clock-owed, so the condition is polled for.
    ok(
        &router,
        "StopTask",
        json!({ "cluster": CLUSTER, "task": enabled.clone(), "reason": "done" }),
        "the stop",
    )
    .await;
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let task = describe_task(&router, &enabled, "the stopped task").await;
        if task["lastStatus"].as_str() == Some("STOPPED") {
            assert_eq!(
                agent_status(&task, FIRST_CONTAINER, "the stopped task").as_deref(),
                Some("STOPPED"),
                "expected the agent of a stopped container to be stopped too, got {task}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "expected the stopped task to reach STOPPED, got {task}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    let (code, message) = refusal(
        &router,
        "ExecuteCommand",
        json!({
            "cluster": CLUSTER,
            "task": enabled,
            "command": "/bin/sh",
            "interactive": true,
        }),
        "a stopped task",
    )
    .await;
    assert_eq!(
        (code.as_str(), message.as_str()),
        ("InvalidParameterException", NOT_AVAILABLE),
        "expected the agent-not-running refusal for a stopped task, got {code:?} with \
         {message:?}"
    );
}
