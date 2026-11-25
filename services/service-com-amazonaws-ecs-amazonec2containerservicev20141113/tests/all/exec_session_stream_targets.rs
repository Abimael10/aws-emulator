// SPDX-License-Identifier: CC0-1.0

//! WHICH container an execute-command session is a session of, and what the emulator answers
//! for other tasks while one session's stream is being carried.
//!
//! # The rule under test
//!
//! > A session `ExecuteCommand` opens is a session of ONE container — the one the request's
//! > `container` names, or a task's only container when the request names none, because "a
//! > container name only needs to be specified for tasks containing multiple containers" — and
//! > every session of the store is carried on the ONE data channel this emulator serves, each
//! > under an id and a token of its own, with its `streamUrl` naming it. A session being
//! > carried settles nothing about any other call: a task launched without
//! > `enableExecuteCommand` still earns the one sentence that states both agent conditions,
//! > and the stream that was being carried keeps carrying what its client types.
//!
//! # What the rows span
//!
//! The forms the `container` member takes for a task holding one container (absent, and naming
//! that container); the FORM of the line a client types into a carried session — a value
//! computed from a variable the session holds, a loop over words, a question about the
//! container's own filesystem, a file written and read back through a text filter, and the
//! status of the line before it; and the sizes a client states for its terminal.
//!
//! Nothing is asserted off the store: every session is opened through `ExecuteCommand`'s own
//! wire call, and every byte is read off a real connection to the announced channel, framed by
//! the one Session Manager client these suites share
//! ([`crate::exec_session_data_channel::dial`]).

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    cluster_with_capacity, list, ok, poll_until, refusal, service, string, ACCOUNT,
};
use crate::exec_session_data_channel::{
    agent_status, dial, long_running_container, session_of, Channel,
};

/// The cluster every task of this suite runs in.
const CLUSTER: &str = "execstreamtargets";

/// The task-definition family they run, with ONE container — the shape whose sessions need no
/// `container` named.
const FAMILY: &str = "execstreamtargetfamily";

/// That container's name.
const CONTAINER: &str = "app";

/// The status a started task and a connected agent both report.
const RUNNING: &str = "RUNNING";

/// The command a client asks an interactive session for.
const SHELL: &str = "/bin/sh";

/// `InvalidParameterException$message` for a task no session can be opened on: fixed text,
/// stating in one sentence both conditions that leave a task without a running
/// execute-command agent.
const NOT_AVAILABLE: &str =
    "The execute command failed because execute command was not enabled when the task was \
     run or the execute command agent isn't running. Wait and try again or run a new task \
     with execute command enabled and try again.";

/// A router holding one cluster with capacity and one revision of [`FAMILY`] declaring a task
/// role — an exec-enabled launch needs one.
async fn fixture() -> MultiServiceRouter {
    let router = service();
    cluster_with_capacity(&router, CLUSTER, "fixture").await;
    ok(
        &router,
        "RegisterTaskDefinition",
        json!({
            "family": FAMILY,
            "taskRoleArn": format!("arn:aws:iam::{ACCOUNT}:role/execStreamTargetTaskRole"),
            "containerDefinitions": [long_running_container(CONTAINER)],
        }),
        "fixture",
    )
    .await;
    router
}

/// One task read back through `DescribeTasks`.
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

/// One task of [`FAMILY`], launched with execute command on or off, and polled until it is
/// RUNNING — with the agent of its container connected when the launch turned the feature on,
/// which is the condition a session is opened on.
///
/// Both the start and the agent's connection are owed by the CLOCK, so the condition is polled
/// for rather than slept on ([`poll_until`]).
async fn launched_task(router: &MultiServiceRouter, enabled: bool, case: &str) -> String {
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
    let task_arn = string(
        tasks
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: expected one launched task, got {body}")),
        "taskArn",
        case,
    );
    let wanted = if enabled {
        "the task to reach RUNNING with its execute-command agent connected"
    } else {
        "the task to reach RUNNING"
    };
    poll_until(
        case,
        wanted,
        || describe_task(router, &task_arn, case),
        |task| {
            task["lastStatus"].as_str() == Some(RUNNING)
                && (!enabled || agent_status(task, CONTAINER, case).as_deref() == Some(RUNNING))
        },
    )
    .await;
    task_arn
}

/// An `ExecuteCommand` request on `task_arn`, naming `container` when the row names one.
fn session_request(task_arn: &str, container: Option<&str>) -> Value {
    let mut request = json!({
        "cluster": CLUSTER,
        "task": task_arn,
        "command": SHELL,
        "interactive": true,
    });
    if let Some(name) = container {
        request["container"] = json!(name);
    }
    request
}

/// The `host:port` an announced `streamUrl` is served on.
fn authority_of(stream_url: &str, case: &str) -> String {
    stream_url
        .strip_prefix("wss://")
        .and_then(|rest| rest.split('/').next())
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a wss:// authority, got {stream_url:?}")
        })
        .to_string()
}

/// Type `line` into a carried session and read the stream until it carries `wanted`.
async fn typed(channel: &mut Channel, case: &str, line: &str, wanted: &str) {
    channel.type_line(case, line).await;
    let read = channel.read_until(case, wanted).await;
    assert!(
        read.contains(wanted),
        "case {case:?}: expected the stream to carry {wanted:?}, got {read:?}"
    );
}

#[tokio::test]
async fn a_session_of_a_single_container_task_needs_no_container_named() {
    let router = fixture().await;
    let task_arn = launched_task(&router, true, "a task run with execute command").await;
    let task_id = task_arn.rsplit('/').next().unwrap_or_default().to_string();

    // The rows span the forms the `container` member takes for a task that holds ONE
    // container: absent, which is the form the member's own documentation allows, and naming
    // that container.
    let rows: [(&str, Option<&str>); 2] = [
        ("no container named", None),
        ("the only container named", Some(CONTAINER)),
    ];
    let mut opened: Vec<(String, String, String)> = Vec::new();
    for (row, named) in rows {
        let case = format!("a session of a single-container task with {row}");
        let answer = ok(
            &router,
            "ExecuteCommand",
            session_request(&task_arn, named),
            &case,
        )
        .await;

        // Both forms open a session of the task's one container, reported off the resources
        // the call resolved rather than off anything the client sent.
        assert_eq!(
            answer["containerName"].as_str(),
            Some(CONTAINER),
            "case {case:?}: expected the task's only container, got {answer}"
        );
        assert_eq!(
            answer["taskArn"].as_str(),
            Some(task_arn.as_str()),
            "case {case:?}: expected the task the request named, got {answer}"
        );
        assert_eq!(
            answer["interactive"].as_bool(),
            Some(true),
            "case {case:?}: Amazon ECS only initiates interactive sessions, got {answer}"
        );
        let container_arn = string(&answer, "containerArn", &case);
        assert!(
            container_arn.contains("container/") && container_arn.contains(&task_id),
            "case {case:?}: expected the ARN of a container of this task, got \
             {container_arn:?}"
        );
        let cluster_arn = string(&answer, "clusterArn", &case);
        assert!(
            cluster_arn.ends_with(&format!("cluster/{CLUSTER}")),
            "case {case:?}: expected the cluster the request named, got {cluster_arn:?}"
        );

        // And the session reported is one that is CARRIED: a line typed into the announced
        // channel runs in that container and comes back on it.
        let (session_id, stream_url, token) = session_of(&answer, &case);
        let mut channel = dial(&case, &stream_url, &token)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: the session's channel must carry it"));
        let marker = format!("opened-with-{}", row.replace(' ', "-"));
        typed(&mut channel, &case, &format!("echo {marker}"), &marker).await;
        opened.push((session_id, stream_url, token));
    }

    // Every session of the store is carried on the ONE channel this emulator serves, each
    // under an id and a token of its own, and each `streamUrl` names the session it carries.
    let (first_id, first_url, first_token) = &opened[0];
    let (second_id, second_url, second_token) = &opened[1];
    assert_ne!(
        first_id, second_id,
        "expected every session to carry an id of its own, got {first_id:?} twice"
    );
    assert_ne!(
        first_token, second_token,
        "expected every session to carry a token of its own"
    );
    assert_eq!(
        authority_of(first_url, "the first session"),
        authority_of(second_url, "the second session"),
        "expected one channel to serve every session of the store, got {first_url:?} and \
         {second_url:?}"
    );
    for (session_id, stream_url) in [(first_id, first_url), (second_id, second_url)] {
        assert!(
            stream_url.contains(session_id),
            "expected the channel to name its session, got {stream_url:?} for {session_id:?}"
        );
    }
}

#[tokio::test]
async fn a_carried_session_runs_every_form_of_line_a_client_types() {
    let router = fixture().await;
    let task_arn = launched_task(&router, true, "a task run with execute command").await;
    let case = "a carried session";
    let answer = ok(
        &router,
        "ExecuteCommand",
        session_request(&task_arn, Some(CONTAINER)),
        case,
    )
    .await;
    let (_, stream_url, token) = session_of(&answer, case);
    let mut channel = dial(case, &stream_url, &token)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: the session's channel must carry it"));

    // The rows differ in the FORM of the line, not in its words: each is run by the
    // container's own session, so what comes back is what that session computed.
    let rows: [(&str, &str, &str); 5] = [
        (
            "a value computed from a variable the session holds",
            "WORD=queued; echo value-$WORD",
            "value-queued",
        ),
        (
            "a loop over words",
            "for part in alpha beta; do echo loop-$part; done",
            "loop-beta",
        ),
        (
            "a question about the container's own filesystem",
            "if [ -f /tmp/exec-targets-absent ]; then echo found-it; else echo no-such-file; fi",
            "no-such-file",
        ),
        (
            "a file written and read back through a filter",
            "printf 'alpha\\nbeta\\n' > /tmp/exec-targets; cat /tmp/exec-targets | grep beta",
            "beta",
        ),
        (
            "the status of the line before it",
            "true; echo status-$?",
            "status-0",
        ),
    ];
    for (row, line, expected) in rows {
        let case = format!("{case}: {row}");
        typed(&mut channel, &case, line, expected).await;
    }

    // The terminal the session runs in is the one the CLIENT states, so a size stated on the
    // channel is the size the session's own report answers. The rows span the sizes a client
    // resizes to, read back through both capabilities a terminal report names.
    for (rows_, columns) in [(47u32, 173u32), (12, 300)] {
        let case = format!("{case}: a terminal of {rows_} by {columns}");
        channel.resize(&case, rows_, columns).await;
        typed(&mut channel, &case, "tput lines", &rows_.to_string()).await;
        typed(&mut channel, &case, "tput cols", &columns.to_string()).await;
        typed(
            &mut channel,
            &case,
            "stty size",
            &format!("{rows_} {columns}"),
        )
        .await;
    }
}

#[tokio::test]
async fn a_task_without_the_flag_is_refused_while_a_session_is_carried() {
    let router = fixture().await;
    let enabled = launched_task(&router, true, "a task run with execute command").await;
    let disabled = launched_task(&router, false, "a task run without execute command").await;

    let case = "the session of the exec-enabled task";
    let answer = ok(
        &router,
        "ExecuteCommand",
        session_request(&enabled, Some(CONTAINER)),
        case,
    )
    .await;
    let (_, stream_url, token) = session_of(&answer, case);
    let mut channel = dial(case, &stream_url, &token)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: the session's channel must carry it"));
    typed(&mut channel, case, "echo carrying-before", "carrying-before").await;

    // A task launched WITHOUT the flag carries no such agent, whatever another task's session
    // is doing: it earns the one sentence that states both conditions.
    let without = "a session asked for on the task run without execute command";
    let task = describe_task(&router, &disabled, without).await;
    assert!(
        agent_status(&task, CONTAINER, without).is_none(),
        "case {without:?}: expected a task launched without the flag to carry no \
         execute-command agent, got {task}"
    );
    let (code, message) = refusal(
        &router,
        "ExecuteCommand",
        session_request(&disabled, Some(CONTAINER)),
        without,
    )
    .await;
    assert_eq!(
        (code.as_str(), message.as_str()),
        ("InvalidParameterException", NOT_AVAILABLE),
        "case {without:?}: expected the one sentence that states both agent conditions, got \
         {code:?} with {message:?}"
    );

    // And the stream that was being carried keeps carrying: the refusal was about the other
    // task.
    typed(&mut channel, case, "echo carrying-after", "carrying-after").await;
}
