// SPDX-License-Identifier: CC0-1.0

//! Draining a container instance RELOCATES the service task it was running onto another host.
//!
//! RULE under test: **`UpdateContainerInstancesState` to `DRAINING` takes a host out of
//! service, and the service scheduler then moves the work off it** — "when you set a container
//! instance to DRAINING, Amazon ECS prevents new tasks from being scheduled for placement on
//! the container instance and replacement service tasks are started on other container
//! instances in the cluster if the resources are available" (`UpdateContainerInstancesState`).
//! So the task the drained host ran is STOPPED by the scheduler
//! (`Task$stopCode` `ServiceSchedulerInitiated`, "the service scheduler initiated the stop"), a
//! REPLACEMENT task with a new `taskArn` runs on the remaining `ACTIVE` host, the service is
//! back at its desired count with nothing pending, and the drained host stays `DRAINING` with
//! no running and no pending tasks of its own.
//!
//! Why the suite lives beside the router: the two hosts are Amazon EC2's, launched through that
//! service's own `RunInstances` with the instance profile and the user data the documented way
//! of joining a cluster carries ("echo ECS_CLUSTER=<cluster> >> /etc/ecs/ecs.config"), the role
//! that profile holds is AWS IAM's, and the registration they earn and the relocation they
//! then take part in are Amazon ECS's — so four crates have to agree, which only the assembled
//! emulator shows. Everything is driven through the real operations on one engine, so what is
//! asserted is what each service persisted.
//!
//! The waits here end on the CONDITION a client polls for — the hosts registering, the task
//! reaching `RUNNING`, the replacement appearing — never on a span of the clock; the deadlines
//! are only there so a convergence that never happens fails the case instead of hanging it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, elements, form_encode};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The cluster the hosts join and the service runs in.
const CLUSTER: &str = "drainrelocate";

/// The service, the family its tasks run and the container inside it.
const SERVICE: &str = "drainrelocate-service";
const FAMILY: &str = "drainrelocate-family";

/// The instance role and the profile a launch carries it in, as the documented ECS-on-EC2
/// setup names them.
const INSTANCE_ROLE: &str = "drainrelocateEcsInstanceRole";
const INSTANCE_PROFILE: &str = "drainrelocateEcsInstanceProfile";

/// The managed policy AWS publishes for an ECS container instance's role.
const INSTANCE_ROLE_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AmazonEC2ContainerServiceforEC2Role";

/// The trust policy an instance role carries: Amazon EC2 is what assumes it.
const EC2_TRUST: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow",
 "Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// The instance type both hosts are.
const INSTANCE_TYPE: &str = "t3.micro";

/// The public parameter AWS publishes the ECS-optimized AMI's id under, which is where a
/// launch reads the image its container agent ships in.
const ECS_OPTIMIZED_AMI_PARAMETER: &str =
    "/aws/service/ecs/optimized-ami/amazon-linux-2023/recommended/image_id";

/// The statuses and the stop code the case asserts, spelled out here rather than imported so a
/// wrong constant in the implementation cannot make the case pass.
const ACTIVE: &str = "ACTIVE";
const DRAINING: &str = "DRAINING";
const RUNNING: &str = "RUNNING";
const STOPPED: &str = "STOPPED";
const SERVICE_SCHEDULER_INITIATED: &str = "ServiceSchedulerInitiated";

/// How long one wait keeps looking, and how long it leaves between looks.
const DEADLINE: Duration = Duration::from_secs(120);
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(100);

/// The emulator as it is ASSEMBLED, on one engine: every service the binary serves, which is
/// what makes the four this case drives agree the way they do in production.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);
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

/// One `awsJson1_1` Amazon ECS call that has to succeed, answered with the parsed body.
async fn ecs(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
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
    let parsed: Value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }))
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected ecs {operation} to answer 200, got {status} with {parsed}"
    );
    parsed
}

/// One form-encoded query call that has to succeed, answered with its XML body: the one
/// calling convention Amazon EC2 and AWS IAM are both reached through here.
async fn query(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    // AWS IAM is a global service, so its host carries no Region.
    let host = if signing_name == "iam" {
        "iam.amazonaws.com".to_string()
    } else {
        format!("{signing_name}.{REGION}.amazonaws.com")
    };
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body))
        .expect("a well-formed query request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected {signing_name} {action} to answer 200, got {status} with \
         {payload}"
    );
    payload
}

/// One Amazon EC2 call, under the API version its `ec2Query` body carries.
async fn ec2(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    query(router, case, "ec2", "2016-11-15", action, params).await
}

/// One AWS IAM call, under the API version its query body carries.
async fn iam(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    query(router, case, "iam", "2010-05-08", action, params).await
}

/// The value AWS Systems Manager holds for one public parameter, read the way a client
/// resolves the ECS-optimized AMI before it launches a host.
async fn ssm_parameter(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ssm.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", "AmazonSSM.GetParameter")
        .header("authorization", authorization("ssm"))
        .body(Bytes::from(
            serde_json::to_vec(&json!({"Name": name})).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = serde_json::from_slice(&payload).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "case {case:?}: expected ssm GetParameter for {name} to answer 200, got {status} with \
         {parsed}"
    );
    parsed["Parameter"]["Value"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a value for {name}, got {parsed}")
        })
        .to_string()
}

/// Every container instance of the cluster whose agent is CONNECTED and whose status is the
/// one asked for, as `DescribeContainerInstances` reports them — reached through the listing a
/// client walks first.
async fn capacity(router: &MultiServiceRouter, case: &str, status: &str) -> Vec<Value> {
    let listed = ecs(
        router,
        case,
        "ListContainerInstances",
        json!({ "cluster": CLUSTER, "status": status }),
    )
    .await;
    let arns: Vec<Value> = listed["containerInstanceArns"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if arns.is_empty() {
        return Vec::new();
    }
    let described = ecs(
        router,
        case,
        "DescribeContainerInstances",
        json!({ "cluster": CLUSTER, "containerInstances": arns }),
    )
    .await;
    described["containerInstances"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|instance| instance["agentConnected"].as_bool() == Some(true))
        .collect()
}

/// The service's RUNNING tasks, as a client reads them: `ListTasks` narrowed to the service
/// and the desired status, then `DescribeTasks` over what it answered.
async fn running_tasks(router: &MultiServiceRouter, case: &str) -> Vec<Value> {
    let listed = ecs(
        router,
        case,
        "ListTasks",
        json!({"cluster": CLUSTER, "serviceName": SERVICE, "desiredStatus": RUNNING}),
    )
    .await;
    let arns: Vec<Value> = listed["taskArns"].as_array().cloned().unwrap_or_default();
    if arns.is_empty() {
        return Vec::new();
    }
    let described = ecs(
        router,
        case,
        "DescribeTasks",
        json!({"cluster": CLUSTER, "tasks": arns}),
    )
    .await;
    described["tasks"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|task| task["lastStatus"].as_str() == Some(RUNNING))
        .collect()
}

/// The ARN of one task or container instance record.
fn arn_of(record: &Value, member: &str) -> String {
    record[member].as_str().unwrap_or_default().to_string()
}

/// Stand two hosts up as ACTIVE, agent-connected container instances of the cluster, and
/// answer their container-instance ARNs.
async fn two_registered_hosts(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    iam(
        router,
        case,
        "CreateRole",
        &[
            ("RoleName", INSTANCE_ROLE),
            ("AssumeRolePolicyDocument", EC2_TRUST),
        ],
    )
    .await;
    iam(
        router,
        case,
        "AttachRolePolicy",
        &[("RoleName", INSTANCE_ROLE), ("PolicyArn", INSTANCE_ROLE_POLICY)],
    )
    .await;
    let profile = iam(
        router,
        case,
        "CreateInstanceProfile",
        &[("InstanceProfileName", INSTANCE_PROFILE)],
    )
    .await;
    let profile_arn = element(&profile, "Arn");
    iam(
        router,
        case,
        "AddRoleToInstanceProfile",
        &[
            ("InstanceProfileName", INSTANCE_PROFILE),
            ("RoleName", INSTANCE_ROLE),
        ],
    )
    .await;

    // The account's DEFAULT VPC and its subnet, which is where a client that picked no network
    // of its own places a host.
    let vpc = element(
        &ec2(
            router,
            case,
            "DescribeVpcs",
            &[("Filter.1.Name", "isDefault"), ("Filter.1.Value.1", "true")],
        )
        .await,
        "vpcId",
    );
    let subnets = elements(
        &ec2(
            router,
            case,
            "DescribeSubnets",
            &[("Filter.1.Name", "vpc-id"), ("Filter.1.Value.1", &vpc)],
        )
        .await,
        "subnetId",
    );
    let subnet = subnets.first().cloned().unwrap_or_else(|| {
        panic!("case {case:?}: expected the default VPC to hold a subnet, got {subnets:?}")
    });
    let image = ssm_parameter(router, case, ECS_OPTIMIZED_AMI_PARAMETER).await;

    // The user data a launch carries: "user data must be base64-encoded"
    // (`RunInstancesRequest$UserData`), which is what an SDK does to the shell line the
    // documented way of joining a cluster writes.
    let user_data = ::aws_smithy_types::base64::encode(format!(
        "#!/bin/bash\necho ECS_CLUSTER={CLUSTER} >> /etc/ecs/ecs.config\n"
    ));
    let mut instances = Vec::new();
    for _ in 0..2 {
        let launched = ec2(
            router,
            case,
            "RunInstances",
            &[
                ("ImageId", &image),
                ("InstanceType", INSTANCE_TYPE),
                ("SubnetId", &subnet),
                ("MinCount", "1"),
                ("MaxCount", "1"),
                ("UserData", &user_data),
                ("IamInstanceProfile.Arn", &profile_arn),
            ],
        )
        .await;
        instances.extend(elements(&launched, "instanceId"));
    }
    assert_eq!(
        instances.len(),
        2,
        "case {case:?}: expected the launches to mint two hosts, got {instances:?}"
    );

    // The registration is owed by the CLOCK — the agent on the instance registers it, with no
    // client request in between — so the wait is for the condition a client polls for.
    let deadline = Instant::now() + DEADLINE;
    let mut registered = capacity(router, case, ACTIVE).await;
    while registered.len() != 2 && Instant::now() < deadline {
        ::tokio::time::sleep(LOOK_AGAIN_AFTER).await;
        registered = capacity(router, case, ACTIVE).await;
    }
    assert_eq!(
        registered.len(),
        2,
        "case {case:?}: expected the hosts {instances:?} to register as {ACTIVE}, connected \
         container instances, got {registered:?}"
    );
    registered
        .iter()
        .map(|instance| arn_of(instance, "containerInstanceArn"))
        .collect()
}

#[tokio::test]
async fn a_draining_host_has_its_service_task_relocated_onto_the_other_host() {
    let case = "an EC2 service of one task on a two-host cluster, one host drained";
    let router = emulator();
    ecs(&router, case, "CreateCluster", json!({"clusterName": CLUSTER})).await;
    let hosts = two_registered_hosts(&router, case).await;

    let registered = ecs(
        &router,
        case,
        "RegisterTaskDefinition",
        json!({
            "family": FAMILY,
            "containerDefinitions": [{
                "name": "app",
                "image": "busybox",
                "cpu": 64,
                "memory": 64,
                "command": ["sh", "-c", "sleep 1800"],
                "essential": true,
            }],
        }),
    )
    .await;
    let revision = arn_of(&registered["taskDefinition"], "taskDefinitionArn");

    ecs(
        &router,
        case,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": SERVICE,
            "taskDefinition": revision,
            "desiredCount": 1,
            "launchType": "EC2",
            "deploymentConfiguration": {"minimumHealthyPercent": 100, "maximumPercent": 200},
        }),
    )
    .await;

    // The scheduler places the service's single task on one of the two hosts.
    let deadline = Instant::now() + DEADLINE;
    let mut placed = running_tasks(&router, case).await;
    while placed.len() != 1 && Instant::now() < deadline {
        ::tokio::time::sleep(LOOK_AGAIN_AFTER).await;
        placed = running_tasks(&router, case).await;
    }
    assert_eq!(
        placed.len(),
        1,
        "case {case:?}: expected the service's single task to reach {RUNNING} on a registered \
         host, got {placed:?}"
    );
    let original_task = arn_of(&placed[0], "taskArn");
    let drained_host = arn_of(&placed[0], "containerInstanceArn");
    assert!(
        hosts.contains(&drained_host),
        "case {case:?}: expected the task to be placed on one of the registered hosts \
         {hosts:?}, got {drained_host:?}"
    );
    let other_host = hosts
        .iter()
        .find(|arn| **arn != drained_host)
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a second host beside {drained_host:?}, got {hosts:?}")
        });

    // The drain itself: accepted, with nothing in `failures`.
    let answered = ecs(
        &router,
        case,
        "UpdateContainerInstancesState",
        json!({
            "cluster": CLUSTER,
            "containerInstances": [&drained_host],
            "status": DRAINING,
        }),
    )
    .await;
    let failures = answered["failures"].as_array().cloned().unwrap_or_default();
    assert!(
        failures.is_empty(),
        "case {case:?}: expected the drain to report no failures, got {failures:?}"
    );
    assert_eq!(
        answered["containerInstances"][0]["status"].as_str(),
        Some(DRAINING),
        "case {case:?}: expected the drain to answer the host as {DRAINING}, got {answered}"
    );

    // ...and then the relocation the scheduler owes: one RUNNING task, on the OTHER host,
    // under a new task ARN.
    let settled = |tasks: &[Value]| {
        tasks.len() == 1
            && arn_of(&tasks[0], "containerInstanceArn") == other_host
            && arn_of(&tasks[0], "taskArn") != original_task
    };
    let deadline = Instant::now() + DEADLINE;
    let mut relocated = running_tasks(&router, case).await;
    while !settled(&relocated) && Instant::now() < deadline {
        ::tokio::time::sleep(LOOK_AGAIN_AFTER).await;
        relocated = running_tasks(&router, case).await;
    }
    assert!(
        settled(&relocated),
        "case {case:?}: expected exactly one {RUNNING} task on {other_host:?} under a task ARN \
         other than {original_task:?}, got {relocated:?}"
    );

    // The original task was stopped BY THE SCHEDULER, on the host that was drained.
    let described = ecs(
        &router,
        case,
        "DescribeTasks",
        json!({"cluster": CLUSTER, "tasks": [&original_task]}),
    )
    .await;
    let original = described["tasks"][0].clone();
    assert_eq!(
        (
            original["lastStatus"].as_str(),
            original["stopCode"].as_str(),
            original["containerInstanceArn"].as_str(),
        ),
        (
            Some(STOPPED),
            Some(SERVICE_SCHEDULER_INITIATED),
            Some(drained_host.as_str()),
        ),
        "case {case:?}: expected the original task {STOPPED} with stop code \
         {SERVICE_SCHEDULER_INITIATED} on {drained_host:?}, got {original}"
    );

    // The service is back at its desired count with nothing pending...
    let services = ecs(
        &router,
        case,
        "DescribeServices",
        json!({"cluster": CLUSTER, "services": [SERVICE]}),
    )
    .await;
    let service = services["services"][0].clone();
    assert_eq!(
        (
            service["runningCount"].as_i64(),
            service["pendingCount"].as_i64(),
        ),
        (Some(1), Some(0)),
        "case {case:?}: expected the service back at one running task and none pending, got \
         {service}"
    );

    // ...and the drained host keeps that status, carrying no work of its own.
    let hosted = ecs(
        &router,
        case,
        "DescribeContainerInstances",
        json!({"cluster": CLUSTER, "containerInstances": [&drained_host]}),
    )
    .await;
    let host = hosted["containerInstances"][0].clone();
    assert_eq!(
        (
            host["status"].as_str(),
            host["runningTasksCount"].as_i64(),
            host["pendingTasksCount"].as_i64(),
        ),
        (Some(DRAINING), Some(0), Some(0)),
        "case {case:?}: expected the drained host to stay {DRAINING} with no running or \
         pending tasks, got {host}"
    );
}
