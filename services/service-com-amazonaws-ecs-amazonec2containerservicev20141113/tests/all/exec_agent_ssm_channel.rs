// SPDX-License-Identifier: CC0-1.0

//! Whether a task's execute-command AGENT connects at all: the IAM question its own
//! connection turns on, driven end to end against the service that holds the policies.
//!
//! # The rule under test
//!
//! > The agent Amazon ECS runs inside the containers of an exec-enabled task opens its
//! > Systems Manager control and data channels AS THE TASK ROLE, so the agent of a task
//! > whose role does not authorize those channel actions
//! > (`ssmmessages:CreateControlChannel`, `ssmmessages:CreateDataChannel`,
//! > `ssmmessages:OpenControlChannel`, `ssmmessages:OpenDataChannel`) never reaches
//! > `lastStatus` `RUNNING`, and `ExecuteCommand` on it earns the one sentence Amazon ECS
//! > states both agent conditions in. WITHHOLDING needs an ANSWER about the role: a role this
//! > account's IAM holds nothing under is no evidence about what its agent may do, so such a
//! > task's agent connects.
//!
//! # What the rows span
//!
//! The policy shapes a task role can carry over those four actions: all four allowed, the
//! service wildcard, only some of them, one of them denied beside an allow, a grant about
//! another service entirely, and a role the account does not hold at all. Each row is a task
//! of its own, and each is judged twice — on the agent `DescribeTasks` reports and on what
//! `ExecuteCommand` answers — so the one decision is observable from both.
//!
//! Every value is read back through the ECS front door, and every role and policy is created
//! through IAM's OWN API rather than written into its store.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    cluster_with_capacity, iam, list, ok, refusal, string, ACCOUNT, SIGNED_REGION,
};
use crate::task_lifecycle::container;

/// The cluster every row places into.
const CLUSTER: &str = "execchannel";

/// The container every revision of this suite declares.
const CONTAINER: &str = "app";

/// The managed agent the execute command feature runs inside a container
/// (`smithy_query(query="shape",shape="ManagedAgentName")` -> `ExecuteCommandAgent`).
const EXECUTE_COMMAND_AGENT: &str = "ExecuteCommandAgent";

/// The status an agent that has connected reports, and the one an agent that has not started
/// reports.
const RUNNING: &str = "RUNNING";
const PENDING: &str = "PENDING";

/// The four actions the agent opens its Systems Manager channels with, which the task role
/// has to allow (`state::SSM_MESSAGES_CHANNEL_ACTIONS`, from Amazon ECS Exec's documented
/// task-role requirement).
const CHANNEL_ACTIONS: [&str; 4] = [
    "ssmmessages:CreateControlChannel",
    "ssmmessages:CreateDataChannel",
    "ssmmessages:OpenControlChannel",
    "ssmmessages:OpenDataChannel",
];

/// The service principal a task role is assumed by, and the action a trust policy allows it.
const ECS_TASKS_PRINCIPAL: &str = "ecs-tasks.amazonaws.com";
const ASSUME_ROLE: &str = "sts:AssumeRole";

/// `InvalidParameterException$message` for a task no session can be opened on: fixed text,
/// stating both conditions that leave a task without a running execute-command agent.
const NOT_AVAILABLE: &str =
    "The execute command failed because execute command was not enabled when the task was \
     run or the execute command agent isn't running. Wait and try again or run a new task \
     with execute command enabled and try again.";

/// How often a row re-reads a task it is waiting on, and how long it keeps trying: the start
/// and the agent's connection are both clock-owed, so the condition is polled for.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const POLL_BUDGET: Duration = Duration::from_secs(60);

/// What one row's task role states about the channel actions.
#[derive(Debug, Clone, Copy)]
enum Grant {
    /// A role this account's IAM holds nothing under.
    NoRecord,
    /// An `Allow` over the actions named.
    Allow(&'static [&'static str]),
    /// An `Allow` over all four, beside a `Deny` of the action named.
    AllowButDeny(&'static str),
}

/// One row: the policy its task role carries, and whether its agent therefore connects.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    grant: Grant,
    /// Whether the agent of this row's task reaches [`RUNNING`].
    connects: bool,
}

/// The service wildcard, which covers every action of the service and so every channel action.
const SERVICE_WILDCARD: [&str; 1] = ["ssmmessages:*"];

/// Only the two channels the agent CREATES, which is half of what it needs.
const ONLY_THE_CREATES: [&str; 2] = [
    "ssmmessages:CreateControlChannel",
    "ssmmessages:CreateDataChannel",
];

/// A grant about another service entirely: the role carries a document, and nothing in it
/// allows the agent's channel actions.
const ANOTHER_SERVICE: [&str; 1] = ["s3:GetObject"];

const ROWS: [Row; 6] = [
    Row {
        case: "a role allowing every channel action",
        grant: Grant::Allow(&CHANNEL_ACTIONS),
        connects: true,
    },
    Row {
        case: "a role allowing the service wildcard",
        grant: Grant::Allow(&SERVICE_WILDCARD),
        connects: true,
    },
    Row {
        case: "a role this account's IAM holds nothing under",
        grant: Grant::NoRecord,
        connects: true,
    },
    Row {
        case: "a role allowing only the channels the agent creates",
        grant: Grant::Allow(&ONLY_THE_CREATES),
        connects: false,
    },
    Row {
        case: "a role whose grant is about another service",
        grant: Grant::Allow(&ANOTHER_SERVICE),
        connects: false,
    },
    Row {
        case: "a role denying one channel action beside the allow",
        grant: Grant::AllowButDeny("ssmmessages:OpenDataChannel"),
        connects: false,
    },
];

/// A router carrying Amazon ECS, IAM — which HOLDS the task roles the decision is made
/// against — and Systems Manager, which HOLDS the sessions an execute-command call opens,
/// over ONE shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_ssm_amazonssm::EmulatedService::new(engine),
    );
    router
}

/// An identity-based policy document with one statement per entry: the effect over the
/// actions, for every resource.
fn policy(statements: &[(&str, &[&str])]) -> String {
    let statements: Vec<Value> = statements
        .iter()
        .map(|(effect, actions)| {
            json!({ "Effect": effect, "Action": actions, "Resource": "*" })
        })
        .collect();
    json!({ "Version": "2012-10-17", "Statement": statements }).to_string()
}

/// The task role one row runs under, created through IAM's own API where the row has IAM hold
/// one — the ARN either way, because a role ARN names a role whether or not this account holds
/// a record of it.
async fn task_role(router: &MultiServiceRouter, index: usize, row: &Row) -> String {
    let name = format!("execChannelRole{index}");
    let arn = format!("arn:aws:iam::{ACCOUNT}:role/{name}");
    let document = match row.grant {
        Grant::NoRecord => return arn,
        Grant::Allow(actions) => policy(&[("Allow", actions)]),
        Grant::AllowButDeny(denied) => {
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
        &[
            ("RoleName", name.clone()),
            ("AssumeRolePolicyDocument", trust),
        ],
        row.case,
    )
    .await;
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", name.clone()),
            ("PolicyName", "execChannel".to_string()),
            ("PolicyDocument", document),
        ],
        row.case,
    )
    .await;
    arn
}

/// Register one revision running under `role_arn` with a container that outlives this suite,
/// and place one exec-enabled task of it: the task's ARN.
async fn exec_enabled_task(
    router: &MultiServiceRouter,
    family: &str,
    role_arn: &str,
    case: &str,
) -> String {
    let mut definition = container(CONTAINER, "public.ecr.aws/nginx/nginx:latest");
    definition["command"] = json!(["sleep", "600"]);
    ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "taskRoleArn": role_arn,
            "containerDefinitions": [definition],
        }),
        case,
    )
    .await;
    let body = ok(
        router,
        "RunTask",
        json!({
            "cluster": CLUSTER,
            "taskDefinition": family,
            "count": 1,
            "enableExecuteCommand": true,
        }),
        case,
    )
    .await;
    string(&list(&body, "tasks", case)[0], "taskArn", case)
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

/// The `lastStatus` the execute-command agent of the task's container reports.
fn agent_status(task: &Value, case: &str) -> Option<String> {
    let container = list(task, "containers", case)
        .into_iter()
        .find(|container| container["name"].as_str() == Some(CONTAINER))?;
    list(&container, "managedAgents", case)
        .into_iter()
        .find(|agent| agent["name"].as_str() == Some(EXECUTE_COMMAND_AGENT))
        .map(|agent| agent["lastStatus"].as_str().unwrap_or_default().to_string())
}

/// That task once it is `RUNNING`, looked for until it is: the start is clock-owed.
async fn running_task(router: &MultiServiceRouter, task_arn: &str, case: &str) -> Value {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let task = describe_task(router, task_arn, case).await;
        if task["lastStatus"].as_str() == Some(RUNNING) {
            return task;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the task to reach RUNNING, got {task}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[tokio::test]
async fn the_agent_connects_only_where_the_task_role_authorizes_the_channel() {
    let router = emulator();
    cluster_with_capacity(&router, CLUSTER, "fixture").await;

    // Every row's task is placed first, so the decisions of the whole table are taken
    // together rather than one wait after another.
    let mut placed: Vec<(usize, &Row, String)> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let role_arn = task_role(&router, index, row).await;
        let family = format!("execchannel{index}");
        let task_arn = exec_enabled_task(&router, &family, &role_arn, row.case).await;
        placed.push((index, row, task_arn));
    }

    for (_, row, task_arn) in &placed {
        let case = row.case;
        running_task(&router, task_arn, case).await;
        // The agent's connection is clock-owed too — it is decided against the policies IAM
        // holds — so a row expecting a CONNECTED agent waits for it. A row expecting none
        // reads the agent after the task is running: an agent reaches RUNNING only where the
        // decision came out in its favour, so there is nothing to wait for.
        let reported = if row.connects {
            let deadline = Instant::now() + POLL_BUDGET;
            loop {
                let task = describe_task(&router, task_arn, case).await;
                let reported = agent_status(&task, case);
                if reported.as_deref() == Some(RUNNING) {
                    break reported;
                }
                assert!(
                    Instant::now() < deadline,
                    "case {case:?}: expected the agent to connect, got {task}"
                );
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        } else {
            let task = describe_task(&router, task_arn, case).await;
            agent_status(&task, case)
        };
        let expected = if row.connects { RUNNING } else { PENDING };
        assert_eq!(
            reported.as_deref(),
            Some(expected),
            "case {case:?}: expected the agent to report {expected}, got {reported:?}"
        );

        // The same decision, read through the operation a client makes: a session is opened on
        // a connected agent, and refused with the one sentence that states both agent
        // conditions where the channel is not authorized.
        let request = json!({
            "cluster": CLUSTER,
            "task": task_arn,
            "command": "/bin/sh",
            "interactive": true,
        });
        if row.connects {
            let answer = ok(&router, "ExecuteCommand", request, case).await;
            let session = &answer["session"];
            let session_id = string(session, "sessionId", case);
            assert!(
                string(session, "streamUrl", case).contains(&session_id)
                    && !string(session, "tokenValue", case).is_empty(),
                "case {case:?}: expected a session carried on this emulator, got {answer}"
            );
        } else {
            let (code, message) = refusal(&router, "ExecuteCommand", request, case).await;
            assert_eq!(
                (code.as_str(), message.as_str()),
                ("InvalidParameterException", NOT_AVAILABLE),
                "case {case:?}: expected the agent-not-running sentence, got {code:?} with \
                 {message:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_connected_session_is_held_by_systems_manager_until_it_ends() {
    let router = emulator();
    cluster_with_capacity(&router, CLUSTER, "fixture").await;
    let case = "a session of a task whose role authorizes the channel";
    let role_arn = task_role(
        &router,
        99,
        &Row {
            case,
            grant: Grant::Allow(&CHANNEL_ACTIONS),
            connects: true,
        },
    )
    .await;
    let task_arn = exec_enabled_task(&router, "execchannelheld", &role_arn, case).await;
    running_task(&router, &task_arn, case).await;
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let task = describe_task(&router, &task_arn, case).await;
        if agent_status(&task, case).as_deref() == Some(RUNNING) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the agent to connect, got {task}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    let answer = ok(
        &router,
        "ExecuteCommand",
        json!({
            "cluster": CLUSTER,
            "task": task_arn,
            "command": "/bin/sh",
            "interactive": true,
        }),
        case,
    )
    .await;
    let session_id = string(&answer["session"], "sessionId", case);

    // An execute-command session IS a Session Manager session, so the service that HOLDS
    // sessions reports it among the account's active ones — read through that service's own
    // API, with the target naming the container the session is a session of.
    let (status, sessions) = ssm(
        &router,
        "DescribeSessions",
        json!({ "State": "Active" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected DescribeSessions to answer 200, got {status} with {sessions}"
    );
    let held = list(&sessions, "Sessions", case)
        .into_iter()
        .find(|session| session["SessionId"].as_str() == Some(session_id.as_str()))
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected Systems Manager to hold {session_id}, got {sessions}")
        });
    let target = string(&held, "Target", case);
    let task_id = task_arn.rsplit('/').next().unwrap_or_default();
    assert!(
        target.starts_with("ecs:") && target.contains(CLUSTER) && target.contains(task_id),
        "case {case:?}: expected the target to name the container of this task, got {target:?}"
    );

    // Ending it THERE ends it here: the session that service has been asked to end is one
    // this one carries no further, so its token opens nothing.
    let (status, ended) = ssm(
        &router,
        "TerminateSession",
        json!({ "SessionId": session_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected TerminateSession to answer 200, got {status} with {ended}"
    );
    let (status, active) = ssm(&router, "DescribeSessions", json!({ "State": "Active" })).await;
    assert_eq!(status, 200, "case {case:?}: expected the active report, got {active}");
    assert!(
        list(&active, "Sessions", case)
            .into_iter()
            .all(|session| session["SessionId"].as_str() != Some(session_id.as_str())),
        "case {case:?}: expected the ended session to leave the active report, got {active}"
    );
}

/// One Systems Manager call, the way a client makes it: this suite mounts that service beside
/// Amazon ECS, because the SESSION an execute-command call opens is held there.
async fn ssm(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ssm.{SIGNED_REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AmazonSSM.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDECSTEST/20260101/{SIGNED_REGION}\
                 /ssm/aws4_request, SignedHeaders=host;x-amz-target, Signature=00"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    (status, parsed)
}
