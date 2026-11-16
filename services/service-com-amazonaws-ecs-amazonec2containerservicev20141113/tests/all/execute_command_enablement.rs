// SPDX-License-Identifier: CC0-1.0

//! What `ExecuteCommand` makes of the task's `enableExecuteCommand` flag, driven end to
//! end over the shared wire harness in [`crate::ecs_wire`].
//!
//! # The rule under test
//!
//! > A session is gated on the TASK first: `Task$enableExecuteCommand` is set by the
//! > placement that ran the task and cannot be turned on afterwards, so a task run without
//! > it is refused with `InvalidParameterException` and the not-enabled sentence, BEFORE
//! > anything about the target is read. A task run WITH it reaches the target the session
//! > is opened against — the Systems Manager Session Manager target the container's
//! > `ExecuteCommandAgent` registers — and a task whose container is no such target is
//! > refused in the other class the operation models, `TargetNotConnectedException`. The
//! > outcome therefore DEPENDS on the flag: exec is disabled by default and enabled by
//! > request, and a client can tell the two refusals apart.
//!
//! # What the rows span
//!
//! Two tasks differing ONLY in `enableExecuteCommand`, in the same cluster, from the same
//! task-definition family, both advanced to `RUNNING`. The table then varies what the
//! `ExecuteCommand` request carries — the task by id and by full ARN, no container named
//! and each of the revision's two containers named, and both values of `interactive` — so
//! the outcome is asserted to be a property of the TASK's flag and of nothing else about
//! the request. The recorded divergence (the same refusal with the flag on as with it off)
//! is the pair of rows naming no container.

use std::time::{Duration, Instant};

use serde_json::json;
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

/// `InvalidParameterException$message` for a task that was not run with execute command
/// turned on: fixed text, with no part taken from the request.
const NOT_ENABLED: &str =
    "The execute command failed because execute command was not enabled when the task was \
     run or the execute command agent isn't running. Wait and try again or run a new task \
     with execute command enabled and try again.";

/// `TargetNotConnectedException$message` for an exec-ENABLED task whose container is no
/// Session Manager target.
const NOT_CONNECTED: &str = "The execute command failed due to an internal error. Try again later.";

/// How often the fixture re-reads a task it is waiting on, and how long it keeps trying:
/// the start is clock-owed, so the condition is polled for rather than slept on, with
/// generous room for a loaded machine.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const POLL_BUDGET: Duration = Duration::from_secs(60);

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
                container(FIRST_CONTAINER, "public.ecr.aws/nginx/nginx:latest"),
                container(SECOND_CONTAINER, "public.ecr.aws/nginx/nginx:latest"),
            ],
        }),
        "fixture",
    )
    .await;
    router
}

/// Run ONE task of [`FAMILY`] with `enable_execute_command`, wait until it is `RUNNING`,
/// and answer its ARN.
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
        let read = ok(
            router,
            "DescribeTasks",
            json!({ "cluster": CLUSTER, "tasks": [arn.clone()] }),
            case,
        )
        .await;
        let described = list(&read, "tasks", case);
        let task = described
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: expected the task back, got {read}"));
        // The flag the placement recorded is the task's own, which is what the gate reads.
        assert_eq!(
            task["enableExecuteCommand"].as_bool().unwrap_or(false),
            enabled,
            "case {case:?}: expected the stored task to report enableExecuteCommand \
             {enabled}, got {read}"
        );
        if task["lastStatus"].as_str() == Some("RUNNING") {
            return arn;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the task to reach RUNNING, got {read}"
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
async fn execute_command_depends_on_the_flag_the_task_was_run_with() {
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
        for (flag, task_arn, expected) in [
            (false, &disabled, ("InvalidParameterException", NOT_ENABLED)),
            (
                true,
                &enabled,
                ("TargetNotConnectedException", NOT_CONNECTED),
            ),
        ] {
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
            let (code, message) = refusal(&router, "ExecuteCommand", request, &case).await;
            assert_eq!(
                (code.as_str(), message.as_str()),
                expected,
                "case {case:?}: expected {expected:?}, got {code:?} with {message:?}"
            );
        }
    }

    // The two outcomes are DIFFERENT, which is the whole of the rule: a client can
    // attribute the refusal to the flag rather than to some other property of the task.
    let common = json!({
        "cluster": CLUSTER,
        "command": "/bin/sh",
        "interactive": true,
    });
    let mut without = common.clone();
    without["task"] = json!(disabled);
    let mut with = common;
    with["task"] = json!(enabled);
    let (code_without, _) = refusal(&router, "ExecuteCommand", without, "the flag off").await;
    let (code_with, _) = refusal(&router, "ExecuteCommand", with, "the flag on").await;
    assert_ne!(
        code_without, code_with,
        "expected enabling execute command to change the outcome, got {code_without:?} both \
         times"
    );
}

#[tokio::test]
async fn the_flag_is_read_after_the_cluster_task_and_container() {
    let router = fixture().await;
    let enabled = running_task(&router, true, "a task run with execute command").await;

    // The resources the request names are resolved BEFORE the flag, so a request that is
    // wrong about one of them is reported on that resource whatever the task's flag says.
    // The rows walk the three resources in the order they are resolved.
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
