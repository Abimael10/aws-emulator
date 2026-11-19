// SPDX-License-Identifier: CC0-1.0

//! What a container instance's own WORK says about deregistering it.
//!
//! RULE under test: a container instance that still holds a task — one placed on it that has
//! not reached `STOPPED` — is deregistered only by a call that forces it. A
//! `DeregisterContainerInstance` without `force` is refused with HTTP 400,
//! `InvalidParameterException` and the fixed sentence `Found running tasks on the instance.`,
//! and leaves the host exactly as it was: still `ACTIVE`, still listed as the cluster's
//! capacity, its task still running. `force` is what asks for those tasks to be orphaned
//! (`DeregisterContainerInstanceRequest$force`: "if you have tasks running on the container
//! instance when you deregister it with the `force` option, these tasks remain running until
//! you terminate the instance or the tasks stop through some other means, but they're
//! orphaned"), so with it the same call deregisters the host — `INACTIVE`, out of the default
//! listing — and leaves the task running. Once every task on the host has stopped there is
//! nothing left to orphan, so the plain call is served.
//!
//! Why the suite lives beside the router: the capacity the rule is about is an Amazon EC2
//! instance's. One row's host is launched through Amazon EC2's OWN `RunInstances` into a
//! subnet that service minted, with the user data that configures the container agent with a
//! cluster, and the rule is asserted on the registration that launch earns — so Amazon ECS
//! and Amazon EC2 have to agree for the row to run at all. The other rows' host registers
//! through the agent's own `RegisterContainerInstance`, which is the same registration
//! reached the other way, so what the rows span is HOW the host came to be as well as what
//! the call carries.
//!
//! The rows differ along what the rule generalises over: whether the call carries `force` (it
//! is absent, `false`, or `true`), whether the host still holds a task (one running, one
//! stopped, none ever), and how the host registered. Every value asserted is read back
//! through `ListContainerInstances`, `DescribeContainerInstances` and `DescribeTasks`.
//!
//! Member and doc source: `smithy_query(query="operation",
//! operation="DeregisterContainerInstance")` — input `{cluster, containerInstance (required),
//! force}`, output `{containerInstance}`, errors `ClientException` /
//! `ClusterNotFoundException` / `InvalidParameterException` at 400 — and
//! `smithy_query(query="doc", shape="DeregisterContainerInstanceRequest", member="force")`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The account id every emulator ARN carries, which an instance identity document has to
/// name for the registration it vouches for to be this caller's.
const ACCOUNT: &str = "000000000000";

/// The error class and the whole sentence a refused deregistration carries. Spelled out here
/// rather than imported, so a wrong wording in the implementation cannot make a row pass.
const INVALID_PARAMETER_EXCEPTION: &str = "InvalidParameterException";
const FOUND_RUNNING_TASKS: &str = "Found running tasks on the instance.";

/// The statuses a container instance reports, of the two these rows read.
const ACTIVE: &str = "ACTIVE";
const INACTIVE: &str = "INACTIVE";

/// The task statuses these rows read.
const RUNNING: &str = "RUNNING";
const STOPPED: &str = "STOPPED";

/// The image every container names, and a program that stays up — so a task that is running
/// is still running when the deregistration is made.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";
const STAYS_UP: [&str; 3] = ["sh", "-c", "while true; do sleep 1; done"];

/// The AMI a launch boots. Well-formed (`ami-<17 hexadecimal digits>`), which is all a launch
/// needs: which AMI carries the container agent is not a fact this emulator publishes.
const IMAGE_ID: &str = "ami-0123456789abcdef0";

/// The instance type the launched host is, and the one the agent-registered hosts report.
const INSTANCE_TYPE: &str = "t3.micro";

/// How long one wait keeps looking before it calls a clock-owed move stalled, and how long it
/// leaves between looks. Each wait gets this budget of its OWN: the waits end on the
/// CONDITION, and the deadline is only there so a move that never happens fails the row
/// instead of hanging it. Nothing here asserts on elapsed time.
const DEADLINE: Duration = Duration::from_secs(120);
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(100);

/// How the row's host came to be one of the cluster's container instances.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Host {
    /// The agent's own registration, which is how a host joins ("this action is only used by
    /// the Amazon ECS agent").
    Agent,
    /// An Amazon EC2 instance launched into the cluster with the user data that configures
    /// the agent with it, whose registration the launch earns.
    Ec2Launch,
}

/// What the row's host still holds when the deregistration is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Work {
    /// A task placed on it that is running.
    RunningTask,
    /// A task placed on it that has been stopped and has reached `STOPPED`.
    StoppedTask,
    /// Nothing was ever placed on it.
    Nothing,
}

/// One row: how the host registered, what it holds, what the call carries, and whether the
/// call is refused.
struct Row {
    case: &'static str,
    cluster: &'static str,
    host: Host,
    work: Work,
    /// `force` as the request carries it: absent, or the value it states.
    force: Option<bool>,
    /// Whether the call is refused, which is also whether the host stays registered.
    refused: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "no force, a task running on the host",
        cluster: "deregforceplain",
        host: Host::Agent,
        work: Work::RunningTask,
        force: None,
        refused: true,
    },
    Row {
        case: "force stated as false, a task running on the host",
        cluster: "deregforcefalse",
        host: Host::Agent,
        work: Work::RunningTask,
        force: Some(false),
        refused: true,
    },
    Row {
        case: "force stated as true, a task running on the host",
        cluster: "deregforcetrue",
        host: Host::Agent,
        work: Work::RunningTask,
        force: Some(true),
        refused: false,
    },
    Row {
        case: "no force, the host's task has stopped",
        cluster: "deregforcestopped",
        host: Host::Agent,
        work: Work::StoppedTask,
        force: None,
        refused: false,
    },
    Row {
        case: "no force, a host that never held a task",
        cluster: "deregforceidle",
        host: Host::Agent,
        work: Work::Nothing,
        force: None,
        refused: false,
    },
    Row {
        case: "no force, a task running on a host launched through Amazon EC2",
        cluster: "deregforcelaunched",
        host: Host::Ec2Launch,
        work: Work::RunningTask,
        force: None,
        refused: true,
    },
];

/// One emulator carrying Amazon ECS and Amazon EC2 — which owns the instances a launched host
/// is one of, and the subnet it is launched into — over ONE shared engine, as the production
/// router mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request; no signature is checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host;x-amz-target, Signature=00"
    )
}

/// One `awsJson1_1` Amazon ECS call, answered with the HTTP status and the parsed body.
async fn ecs(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ecs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AmazonEC2ContainerServiceV20141113.{operation}"),
        )
        .header("authorization", authorization("ecs"))
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }))
    };
    (status, parsed)
}

/// One Amazon ECS call that has to succeed.
async fn ecs_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    let (status, answer) = ecs(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ecs {operation} to answer 200, got {status} with {answer}"
    );
    answer
}

/// The Amazon EC2 API version an `ec2Query` body carries.
const EC2_API_VERSION: &str = "2016-11-15";

/// One `ec2Query` Amazon EC2 call that has to succeed, answered with the XML body: an
/// `application/x-www-form-urlencoded` POST carrying `Action=<Operation>`, with every value
/// form-encoded.
async fn ec2(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version={EC2_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://ec2.{REGION}.amazonaws.com/"))
        .header("host", format!("ec2.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("ec2"))
        .body(Bytes::from(body))
        .expect("a well-formed ec2Query request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected ec2 {action} to answer 200, got {status} with {payload}"
    );
    payload
}

/// The error code a refusal reports, as the short shape name its `__type` carries.
fn error_code(body: &Value) -> String {
    body["__type"]
        .as_str()
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// An EC2 instance identity document, as the agent reads it from the instance metadata
/// service of the machine it runs on.
fn identity_document(instance_id: &str) -> String {
    json!({
        "accountId": ACCOUNT,
        "instanceId": instance_id,
        "region": REGION,
        "instanceType": INSTANCE_TYPE,
        "availabilityZone": format!("{REGION}a"),
    })
    .to_string()
}

/// The container-instance ARNs `ListContainerInstances` reports for `cluster` under its
/// default filter, which "includes container instances set to all states other than
/// INACTIVE".
async fn listed(router: &MultiServiceRouter, case: &str, cluster: &str) -> Vec<String> {
    let body = ecs_ok(
        router,
        case,
        "ListContainerInstances",
        json!({ "cluster": cluster }),
    )
    .await;
    body["containerInstanceArns"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|arn| arn.as_str().map(str::to_string))
        .collect()
}

/// One container instance as `DescribeContainerInstances` reports it.
async fn described(router: &MultiServiceRouter, case: &str, cluster: &str, arn: &str) -> Value {
    let body = ecs_ok(
        router,
        case,
        "DescribeContainerInstances",
        json!({ "cluster": cluster, "containerInstances": [arn] }),
    )
    .await;
    body["containerInstances"][0].clone()
}

/// The cluster of `row` with ONE container instance in it, answered as that instance's ARN.
///
/// The agent's rows register through the operation the agent itself calls; the launched row
/// gets its host from Amazon EC2 and waits for the registration that launch earns, which is
/// exactly what a client polling `ListContainerInstances` after a `RunInstances` waits for.
async fn host_of(router: &MultiServiceRouter, row: &Row) -> String {
    let case = row.case;
    ecs_ok(
        router,
        case,
        "CreateCluster",
        json!({ "clusterName": row.cluster }),
    )
    .await;
    match row.host {
        Host::Agent => {
            let registered = ecs_ok(
                router,
                case,
                "RegisterContainerInstance",
                json!({
                    "cluster": row.cluster,
                    "instanceIdentityDocument": identity_document("i-0c0ffee0c0ffee0c0"),
                    // A registration presents the document's signature beside it; nothing
                    // here signs anything, so this is base64 text of a signature's shape.
                    "instanceIdentityDocumentSignature": "ZXhhbXBsZQ".repeat(35),
                }),
            )
            .await;
            registered["containerInstance"]["containerInstanceArn"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected a registered host, got {registered}")
                })
                .to_string()
        }
        Host::Ec2Launch => {
            let vpc = element(
                &ec2(router, case, "CreateVpc", &[("CidrBlock", "10.80.0.0/16")]).await,
                "vpcId",
            );
            let subnet = element(
                &ec2(
                    router,
                    case,
                    "CreateSubnet",
                    &[
                        ("VpcId", vpc.as_str()),
                        ("CidrBlock", "10.80.0.0/20"),
                        ("AvailabilityZone", format!("{REGION}a").as_str()),
                    ],
                )
                .await,
                "subnetId",
            );
            // The documented way to launch an instance into a cluster: the agent's
            // `ECS_CLUSTER` variable, appended to its configuration file by the launch's user
            // data. The text is the one an instance reads — a launch that carries it base64,
            // as a client's does, is covered by the Amazon ECS crate's own suite.
            let user_data =
                format!("#!/bin/bash\necho ECS_CLUSTER={} >> /etc/ecs/ecs.config\n", row.cluster);
            let launched = ec2(
                router,
                case,
                "RunInstances",
                &[
                    ("ImageId", IMAGE_ID),
                    ("InstanceType", INSTANCE_TYPE),
                    ("SubnetId", subnet.as_str()),
                    ("MinCount", "1"),
                    ("MaxCount", "1"),
                    ("UserData", user_data.as_str()),
                ],
            )
            .await;
            let instance_id = element(&launched, "instanceId");
            registered_host(router, row, &instance_id).await
        }
    }
}

/// Wait until the launched instance `instance_id` is reported as one of `row`'s cluster's
/// container instances — `ACTIVE`, with its agent connected — and answer that container
/// instance's ARN.
async fn registered_host(router: &MultiServiceRouter, row: &Row, instance_id: &str) -> String {
    let case = row.case;
    let deadline = Instant::now() + DEADLINE;
    loop {
        let mut reported = Vec::new();
        for arn in listed(router, case, row.cluster).await {
            let instance = described(router, case, row.cluster, &arn).await;
            if instance["ec2InstanceId"].as_str() == Some(instance_id)
                && instance["status"].as_str() == Some(ACTIVE)
                && instance["agentConnected"].as_bool() == Some(true)
            {
                return arn;
            }
            reported.push(instance);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the instance {instance_id} launched into {} to register \
             as an {ACTIVE}, agent-connected container instance, got {reported:?}",
            row.cluster
        );
        ::tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// Register the revision the rows' tasks run: one container, on the capacity the `EC2` launch
/// type uses.
async fn register_revision(router: &MultiServiceRouter, case: &str, family: &str) {
    ecs_ok(
        router,
        case,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "networkMode": "bridge",
            "requiresCompatibilities": ["EC2"],
            "containerDefinitions": [{
                "name": "app",
                "image": IMAGE,
                "cpu": 64,
                "memory": 64,
                "essential": true,
                "command": STAYS_UP,
            }],
        }),
    )
    .await;
}

/// Start one task of `family` on `host` and wait until it is `RUNNING`, answering its ARN.
async fn running_task(
    router: &MultiServiceRouter,
    row: &Row,
    family: &str,
    host: &str,
) -> String {
    let case = row.case;
    let started = ecs_ok(
        router,
        case,
        "StartTask",
        json!({
            "cluster": row.cluster,
            "containerInstances": [host],
            "taskDefinition": family,
        }),
    )
    .await;
    let task = started["tasks"][0]["taskArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected StartTask to place a task on the \
                                   host, got {started}"))
        .to_string();
    await_task_status(router, row, &task, RUNNING).await;
    task
}

/// Wait until the task `task` reports `wanted` as its `lastStatus`.
async fn await_task_status(router: &MultiServiceRouter, row: &Row, task: &str, wanted: &str) {
    let case = row.case;
    let deadline = Instant::now() + DEADLINE;
    loop {
        let read = task_status(router, case, row.cluster, task).await;
        if read.as_deref() == Some(wanted) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the task {task} to reach {wanted}, last read {read:?}"
        );
        ::tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// The `lastStatus` `DescribeTasks` reports for one task.
async fn task_status(
    router: &MultiServiceRouter,
    case: &str,
    cluster: &str,
    task: &str,
) -> Option<String> {
    let described = ecs_ok(
        router,
        case,
        "DescribeTasks",
        json!({ "cluster": cluster, "tasks": [task] }),
    )
    .await;
    described["tasks"][0]["lastStatus"]
        .as_str()
        .map(str::to_string)
}

#[tokio::test]
async fn a_host_holding_a_task_is_deregistered_only_by_a_call_that_forces_it() {
    let router = emulator();
    let family = "deregforcefamily";
    register_revision(&router, "setup", family).await;

    for row in ROWS {
        let case = row.case;
        let host = host_of(&router, row).await;
        let task = match row.work {
            Work::RunningTask => Some(running_task(&router, row, family, &host).await),
            Work::StoppedTask => {
                let task = running_task(&router, row, family, &host).await;
                ecs_ok(
                    &router,
                    case,
                    "StopTask",
                    json!({ "cluster": row.cluster, "task": task }),
                )
                .await;
                await_task_status(&router, row, &task, STOPPED).await;
                Some(task)
            }
            Work::Nothing => None,
        };

        let mut request = json!({ "cluster": row.cluster, "containerInstance": host });
        if let Some(force) = row.force {
            request["force"] = json!(force);
        }
        let (status, answer) = ecs(&router, "DeregisterContainerInstance", request).await;

        if row.refused {
            assert_eq!(
                status, 400,
                "case {case:?}: expected the deregistration of a host holding a task to be \
                 refused with 400, got {status} with {answer}"
            );
            let code = error_code(&answer);
            assert_eq!(
                code, INVALID_PARAMETER_EXCEPTION,
                "case {case:?}: expected the refusal to carry \
                 {INVALID_PARAMETER_EXCEPTION}, got {code:?} in {answer}"
            );
            assert_eq!(
                answer["message"].as_str(),
                Some(FOUND_RUNNING_TASKS),
                "case {case:?}: expected the refusal's whole message to be \
                 {FOUND_RUNNING_TASKS:?}, got {answer}"
            );
            // The refused call changed nothing: the host is still the cluster's capacity and
            // its task is still running.
            let instance = described(&router, case, row.cluster, &host).await;
            assert_eq!(
                instance["status"].as_str(),
                Some(ACTIVE),
                "case {case:?}: expected the refused deregistration to leave the host \
                 {ACTIVE}, got {instance}"
            );
            assert!(
                listed(&router, case, row.cluster).await.contains(&host),
                "case {case:?}: expected the host to stay in the cluster's listing"
            );
            let held = task.as_deref().expect("a refused row holds a task");
            let read = task_status(&router, case, row.cluster, held).await;
            assert_eq!(
                read.as_deref(),
                Some(RUNNING),
                "case {case:?}: expected the host's task to keep running, got {read:?}"
            );
            continue;
        }

        assert_eq!(
            status, 200,
            "case {case:?}: expected the deregistration to be served with 200, got {status} \
             with {answer}"
        );
        assert_eq!(
            answer["containerInstance"]["containerInstanceArn"].as_str(),
            Some(host.as_str()),
            "case {case:?}: expected the answer to be about the host named, got {answer}"
        );
        assert_eq!(
            answer["containerInstance"]["status"].as_str(),
            Some(INACTIVE),
            "case {case:?}: expected the deregistered host to report {INACTIVE}, got {answer}"
        );
        assert!(
            !listed(&router, case, row.cluster).await.contains(&host),
            "case {case:?}: expected the deregistered host to leave the cluster's listing"
        );
        if row.work == Work::RunningTask {
            // "These tasks remain running ... but they're orphaned": a forced deregistration
            // takes the host out of the cluster and leaves its task exactly as it was.
            let held = task.as_deref().expect("the forced row holds a task");
            let read = task_status(&router, case, row.cluster, held).await;
            assert_eq!(
                read.as_deref(),
                Some(RUNNING),
                "case {case:?}: expected the orphaned task to keep running, got {read:?}"
            );
        }
    }
}
