// SPDX-License-Identifier: CC0-1.0

//! The agent-facing `Submit*StateChange` validation boundary, on the ASSEMBLED emulator.
//!
//! RULE under test: **the agent plane's refusals are reachable on both capacities a cluster
//! has.** A task on the infrastructure AWS manages is allocated its elastic network interface
//! by the call that runs it and reports the attachment inline; its state report carries no
//! verifiable container-instance identity, so it is refused HTTP 403
//! `AccessDeniedException`, and a state the agent plane does not accept for that interface is
//! refused `InvalidParameterException`. An Amazon EC2 instance launched with an ECS instance
//! profile and `ECS_CLUSTER` user data becomes an `ACTIVE`, agent-connected container
//! instance, a `StartTask` onto it reaches `RUNNING`, the agent's RETRY of the stage that task
//! is already at is acknowledged and moves nothing, and a report naming a container the task
//! does not hold is refused `InvalidParameterException` naming that container and the task —
//! leaving the task's container list exactly as it was.
//!
//! Why the suite lives beside the router: the host is Amazon EC2's, launched through that
//! service's own `RunInstances` with the instance profile AWS IAM holds and the image AWS
//! Systems Manager publishes, and the registration it earns and every refusal above are
//! Amazon ECS's — so four crates have to agree, which only the assembled emulator shows.
//! Everything is driven through the real operations on one engine, so what is asserted is
//! what each service persisted.
//!
//! The waits here end on the CONDITION a client polls for — the host registering, the task
//! reaching `RUNNING` — never on a span of the clock; the deadlines are only there so a
//! convergence that never happens fails the case instead of hanging it.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, elements, form_encode};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The account id every emulator ARN carries.
const ACCOUNT: &str = "000000000000";

/// The cluster the whole scenario acts in.
const CLUSTER: &str = "agentboundary";

/// The two families the scenario runs: the Fargate-capable `awsvpc` one, and the one the
/// EC2-backed host starts in the default network mode.
const FARGATE_FAMILY: &str = "agentboundary-fargate";
const EC2_FAMILY: &str = "agentboundary-ec2";

/// The containers those families declare.
const FIRST_CONTAINER: &str = "app";
const SECOND_CONTAINER: &str = "sidecar";

/// The image both families run and the long-running program their containers perform.
const IMAGE: &str = "public.ecr.aws/docker/library/busybox:stable";

/// The instance role and the profile a launch carries it in, as the documented ECS-on-EC2
/// setup names them.
const INSTANCE_ROLE: &str = "agentboundaryEcsInstanceRole";
const INSTANCE_PROFILE: &str = "agentboundaryEcsInstanceProfile";

/// The managed policy AWS publishes for an ECS container instance's role.
const INSTANCE_ROLE_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AmazonEC2ContainerServiceforEC2Role";

/// The trust policy an instance role carries: Amazon EC2 is what assumes it.
const EC2_TRUST: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow",
 "Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// The instance type the host is.
const INSTANCE_TYPE: &str = "t3.micro";

/// The public parameter AWS publishes the ECS-optimized AMI's id under, which is where a
/// launch reads the image its container agent ships in.
const ECS_OPTIMIZED_AMI_PARAMETER: &str =
    "/aws/service/ecs/optimized-ami/amazon-linux-2023/recommended/image_id";

/// The statuses and the acknowledgement the case asserts, spelled out here rather than
/// imported so a wrong constant in the implementation cannot make the case pass.
const ACTIVE: &str = "ACTIVE";
const RUNNING: &str = "RUNNING";
const ACK: &str = "ACK";

/// The type of attachment an `awsvpc` task is allocated.
const ELASTIC_NETWORK_INTERFACE: &str = "ElasticNetworkInterface";

/// The sentence a task state report Amazon ECS cannot attribute to a verified container agent
/// is refused with.
const UNVERIFIED_TASK_REPORT: &str =
    "Fargate task state changes from the agent must have the instance verified";

/// The sentence a state the agent plane does not accept for an attachment is refused with.
const ATTACHMENT_STATUS_NOT_ACCEPTED: &str = "attachmentStatus should be one of [ATTACHED]";

/// The container name the unknown-container report names, which no revision of this scenario
/// declares.
const UNHELD_CONTAINER: &str = "not-a-real-container-on-this-task";

/// How long one wait keeps looking, and how long it leaves between looks.
const DEADLINE: Duration = Duration::from_secs(120);
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(100);

/// The emulator as it is ASSEMBLED, on one engine: every service the binary serves.
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

/// One `awsJson1_1` Amazon ECS call, answered with its HTTP status and parsed body.
async fn ecs_call(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
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
    (status, parsed)
}

/// One Amazon ECS call that has to succeed, answered with the parsed body.
async fn ecs(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    let (status, parsed) = ecs_call(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ecs {operation} to answer 200, got {status} with {parsed}"
    );
    parsed
}

/// One form-encoded query call that has to succeed, answered with its XML body.
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
        .unwrap_or_else(|| panic!("case {case:?}: expected a value for {name}, got {parsed}"))
        .to_string()
}

/// One container definition: the image's own long-running program, which is what keeps the
/// task it belongs to in `RUNNING`.
fn container(name: &str) -> Value {
    json!({
        "name": name,
        "image": IMAGE,
        "cpu": 64,
        "memory": 128,
        "essential": true,
        "command": ["sh", "-c", "sleep 3600"],
    })
}

/// The network the scenario places everything in: the account's DEFAULT VPC, which is where a
/// client that picked no network of its own places a task and a host.
struct Network {
    subnet_id: String,
    security_group_id: String,
}

/// Read that network out of Amazon EC2's own records.
async fn default_network(router: &MultiServiceRouter, case: &str) -> Network {
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
    let subnet_id = subnets.first().cloned().unwrap_or_else(|| {
        panic!("case {case:?}: expected the default VPC to hold a subnet, got {subnets:?}")
    });
    let security_group_id = element(
        &ec2(
            router,
            case,
            "DescribeSecurityGroups",
            &[("Filter.1.Name", "vpc-id"), ("Filter.1.Value.1", &vpc)],
        )
        .await,
        "groupId",
    );
    Network {
        subnet_id,
        security_group_id,
    }
}

/// Create the ECS instance role and the profile a launch carries it in, and answer the
/// profile's name.
async fn instance_profile(router: &MultiServiceRouter, case: &str) -> String {
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
        &[
            ("RoleName", INSTANCE_ROLE),
            ("PolicyArn", INSTANCE_ROLE_POLICY),
        ],
    )
    .await;
    iam(
        router,
        case,
        "CreateInstanceProfile",
        &[("InstanceProfileName", INSTANCE_PROFILE)],
    )
    .await;
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
    INSTANCE_PROFILE.to_string()
}

/// One task, as `DescribeTasks` reports it.
async fn describe_task(router: &MultiServiceRouter, case: &str, task_arn: &str) -> Value {
    let body = ecs(
        router,
        case,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": [task_arn] }),
    )
    .await;
    body["tasks"]
        .as_array()
        .and_then(|tasks| tasks.first().cloned())
        .unwrap_or_else(|| panic!("case {case:?}: expected the task {task_arn} back, got {body}"))
}

/// Every container of `task`, by name, with what a report could have moved about it.
fn container_footprint(task: &Value) -> BTreeMap<String, Value> {
    task["containers"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|container| {
            (
                container["name"].as_str().unwrap_or_default().to_string(),
                json!({
                    "lastStatus": container["lastStatus"],
                    "runtimeId": container["runtimeId"],
                    "exitCode": container["exitCode"],
                    "reason": container["reason"],
                    "managedAgents": container["managedAgents"],
                }),
            )
        })
        .collect()
}

/// The id segment of a task ARN, which is what the agent plane names a task by.
fn task_id_of(task_arn: &str) -> &str {
    task_arn.rsplit('/').next().unwrap_or(task_arn)
}

/// The ARN of an attachment of this account, the form an agent reports one under.
fn attachment_arn(id: &str) -> String {
    format!("arn:aws:ecs:{REGION}:{ACCOUNT}:attachment/{id}")
}

#[tokio::test]
async fn the_agent_state_change_boundary_is_reachable_on_both_capacities() {
    let case = "the agent-facing state-change boundary";
    let router = emulator();
    ecs(&router, case, "CreateCluster", json!({"clusterName": CLUSTER})).await;
    let network = default_network(&router, case).await;

    // ---- the capacity AWS manages ----
    ecs(
        &router,
        case,
        "RegisterTaskDefinition",
        json!({
            "family": FARGATE_FAMILY,
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            "cpu": "256",
            "memory": "512",
            "containerDefinitions": [container(FIRST_CONTAINER)],
        }),
    )
    .await;
    let placed = ecs(
        &router,
        case,
        "RunTask",
        json!({
            "cluster": CLUSTER,
            "taskDefinition": FARGATE_FAMILY,
            "count": 1,
            "launchType": "FARGATE",
            "networkConfiguration": {
                "awsvpcConfiguration": {
                    "subnets": [network.subnet_id],
                    "securityGroups": [network.security_group_id],
                    "assignPublicIp": "ENABLED",
                }
            },
        }),
    )
    .await;
    let fargate_task = placed["tasks"]
        .as_array()
        .and_then(|tasks| tasks.first().cloned())
        .unwrap_or_else(|| panic!("case {case:?}: expected the run to place one task, got {placed}"));
    let fargate_arn = fargate_task["taskArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    // "If the network mode is `awsvpc`, the task is allocated an elastic network interface":
    // the interface is the task's, so the call that ran it reports the attachment INLINE.
    let interface = fargate_task["attachments"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|attachment| attachment["type"].as_str() == Some(ELASTIC_NETWORK_INTERFACE))
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected the run to report the task's elastic network \
                 interface, got {fargate_task}"
            )
        });
    let attachment_id = interface["id"].as_str().unwrap_or_default().to_string();
    assert!(
        !attachment_id.is_empty(),
        "case {case:?}: expected the attachment to carry its id, got {interface}"
    );

    // Such a task runs on no container instance of the account, so no caller can be verified
    // as its agent: its state report is refused HTTP 403.
    let (status, body) = ecs_call(
        &router,
        "SubmitTaskStateChange",
        json!({ "cluster": CLUSTER, "task": fargate_arn, "status": RUNNING }),
    )
    .await;
    assert_eq!(
        (
            status,
            body["__type"]
                .as_str()
                .unwrap_or_default()
                .contains("AccessDeniedException"),
            body["message"].as_str().unwrap_or_default(),
        ),
        (403, true, UNVERIFIED_TASK_REPORT),
        "case {case:?}: expected the unverified-agent refusal, got {status} with {body}"
    );

    // The one state the agent plane accepts for that interface is the attach it completed.
    let (status, body) = ecs_call(
        &router,
        "SubmitAttachmentStateChanges",
        json!({
            "cluster": CLUSTER,
            "attachments": [{
                "attachmentArn": attachment_arn(&attachment_id),
                "status": "NOT_A_REAL_STATUS",
            }],
        }),
    )
    .await;
    assert_eq!(
        (
            status,
            body["__type"]
                .as_str()
                .unwrap_or_default()
                .contains("InvalidParameterException"),
            body["message"].as_str().unwrap_or_default(),
        ),
        (400, true, ATTACHMENT_STATUS_NOT_ACCEPTED),
        "case {case:?}: expected the out-of-set attachment refusal, got {status} with {body}"
    );

    // ---- the capacity the account launched ----
    let profile = instance_profile(&router, case).await;
    let image = ssm_parameter(&router, case, ECS_OPTIMIZED_AMI_PARAMETER).await;
    // "user data must be base64-encoded" (`RunInstancesRequest$UserData`), which is what an
    // SDK does to the shell line the documented way of joining a cluster writes.
    let user_data = ::aws_smithy_types::base64::encode(format!(
        "#!/bin/bash\necho ECS_CLUSTER={CLUSTER} >> /etc/ecs/ecs.config\n"
    ));
    let launched = ec2(
        &router,
        case,
        "RunInstances",
        &[
            ("ImageId", &image),
            ("InstanceType", INSTANCE_TYPE),
            ("SubnetId", &network.subnet_id),
            ("SecurityGroupId.1", &network.security_group_id),
            ("MinCount", "1"),
            ("MaxCount", "1"),
            ("UserData", &user_data),
            ("IamInstanceProfile.Name", &profile),
        ],
    )
    .await;
    let instance_id = element(&launched, "instanceId");

    // The registration is owed by the CLOCK — the agent on the instance registers it, with no
    // client request in between — so the wait is for the condition a client polls for.
    let deadline = Instant::now() + DEADLINE;
    let mut registered = registered_host(&router, case, &instance_id).await;
    while registered.is_none() && Instant::now() < deadline {
        ::tokio::time::sleep(LOOK_AGAIN_AFTER).await;
        registered = registered_host(&router, case, &instance_id).await;
    }
    let host = registered.unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the instance {instance_id} to register as an {ACTIVE}, \
             agent-connected container instance of {CLUSTER}"
        )
    });
    let host_arn = host["containerInstanceArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    ecs(
        &router,
        case,
        "RegisterTaskDefinition",
        json!({
            "family": EC2_FAMILY,
            "containerDefinitions": [
                container(FIRST_CONTAINER),
                container(SECOND_CONTAINER),
            ],
        }),
    )
    .await;
    let started = ecs(
        &router,
        case,
        "StartTask",
        json!({
            "cluster": CLUSTER,
            "taskDefinition": EC2_FAMILY,
            "containerInstances": [host_arn],
        }),
    )
    .await;
    assert_eq!(
        started["failures"].as_array().map(Vec::len).unwrap_or(0),
        0,
        "case {case:?}: expected the start to place the task on the registered host, got \
         {started}"
    );
    let ec2_arn = started["tasks"]
        .as_array()
        .and_then(|tasks| tasks.first())
        .and_then(|task| task["taskArn"].as_str())
        .unwrap_or_else(|| panic!("case {case:?}: expected one started task, got {started}"))
        .to_string();

    // The start is owed by the clock, not by the call.
    let deadline = Instant::now() + DEADLINE;
    let mut task = describe_task(&router, case, &ec2_arn).await;
    while task["lastStatus"].as_str() != Some(RUNNING) && Instant::now() < deadline {
        ::tokio::time::sleep(LOOK_AGAIN_AFTER).await;
        task = describe_task(&router, case, &ec2_arn).await;
    }
    assert_eq!(
        task["lastStatus"].as_str(),
        Some(RUNNING),
        "case {case:?}: expected the started task to reach {RUNNING}, got {task}"
    );
    let version_before = task["version"].clone();

    // The agent RETRIES, so a report of the stage the task is already at is acknowledged and
    // moves nothing — the task's own report and its container's alike.
    for (report_case, operation, request) in [
        (
            "the task's own retry",
            "SubmitTaskStateChange",
            json!({ "cluster": CLUSTER, "task": ec2_arn, "status": RUNNING }),
        ),
        (
            "its container's retry",
            "SubmitContainerStateChange",
            json!({
                "cluster": CLUSTER,
                "task": ec2_arn,
                "containerName": FIRST_CONTAINER,
                "status": RUNNING,
            }),
        ),
    ] {
        let acknowledged = ecs(&router, report_case, operation, request).await;
        assert_eq!(
            acknowledged["acknowledgment"].as_str(),
            Some(ACK),
            "case {report_case:?}: expected the report to be acknowledged, got {acknowledged}"
        );
        let read = describe_task(&router, report_case, &ec2_arn).await;
        assert_eq!(
            (read["lastStatus"].clone(), read["version"].clone()),
            (json!(RUNNING), version_before.clone()),
            "case {report_case:?}: expected an idempotent report to move nothing, got {read}"
        );
    }

    // And the container-name boundary: the refusal names the container the request named and
    // the task it was weighed against, and no record is minted under a name nothing held.
    let before = container_footprint(&describe_task(&router, case, &ec2_arn).await);
    let (status, body) = ecs_call(
        &router,
        "SubmitTaskStateChange",
        json!({
            "cluster": CLUSTER,
            "task": ec2_arn,
            "containers": [{ "containerName": UNHELD_CONTAINER, "status": RUNNING }],
        }),
    )
    .await;
    assert_eq!(
        (
            status,
            body["__type"]
                .as_str()
                .unwrap_or_default()
                .contains("InvalidParameterException"),
            body["message"].as_str().unwrap_or_default(),
        ),
        (
            400,
            true,
            format!(
                "Unable to find container {UNHELD_CONTAINER} for task {}",
                task_id_of(&ec2_arn)
            )
            .as_str()
        ),
        "case {case:?}: expected the unknown-container refusal, got {status} with {body}"
    );
    let after = container_footprint(&describe_task(&router, case, &ec2_arn).await);
    assert_eq!(
        after, before,
        "case {case:?}: expected the container list left as it was, got {after:?}"
    );
    assert!(
        !after.contains_key(UNHELD_CONTAINER),
        "case {case:?}: expected no container minted under {UNHELD_CONTAINER:?}, got {after:?}"
    );
}

/// The container instance of [`CLUSTER`] that carries `instance_id` and is `ACTIVE` with its
/// agent connected, as a client reads it: the listing narrowed to `ACTIVE`, then the describe
/// over what it answered.
async fn registered_host(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> Option<Value> {
    let listed = ecs(
        router,
        case,
        "ListContainerInstances",
        json!({ "cluster": CLUSTER, "status": ACTIVE }),
    )
    .await;
    let arns: Vec<Value> = listed["containerInstanceArns"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if arns.is_empty() {
        return None;
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
        .into_iter()
        .flatten()
        .find(|instance| {
            instance["ec2InstanceId"].as_str() == Some(instance_id)
                && instance["status"].as_str() == Some(ACTIVE)
                && instance["agentConnected"] == json!(true)
        })
        .cloned()
}
