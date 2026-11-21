// SPDX-License-Identifier: CC0-1.0

//! The recorded shared-EFS HANDOVER, in the shape a client drives it in: one Amazon EFS file
//! system carried from one Fargate task to the next, with one revision reused and each run's
//! program sent as that launch's own container override.
//!
//! RULE under test: a task definition volume with an `efsVolumeConfiguration` is the named
//! Amazon EFS file system itself, mounted into every container that lists the volume in
//! `mountPoints` at that entry's `containerPath`, rooted at the path its access point
//! enforces — so the mount is ONE namespace across tasks: what a container of one task writes
//! there, a container of a LATER task reads back byte for byte. A mount point whose `readOnly`
//! is `true` gives that container read-only access ("If this value is `true`, the container
//! has read-only access to the volume"): the kernel refuses every write through it with EROFS,
//! whose text is `Read-only file system`, while the read through the same mount still answers
//! and the volume is left as it was. Each container's own output is delivered through its
//! `awslogs` driver, which is what makes all of it observable to a client.
//!
//! What this suite drives that `all/ecs_efs_volume_mounts.rs` does not: the shape of the
//! recording. ONE read-write revision is reused by three of the four runs (and one read-only
//! revision by the fourth), none of them names a program at all, and each run sends its own
//! program and its own `MARKER` as the launch's `overrides.containerOverrides`
//! (`ContainerOverride$command`/`$environment`) — so what is under test here is the handover
//! over a REUSED revision, where a run's program and the file it leaves behind are the
//! launch's rather than the revision's.
//!
//! Each run is started only once the run before it has STOPPED, which is what makes the
//! handover a handover rather than two containers writing at once. The waits are for the
//! CONDITION, with a deadline only so a move that never happens fails the test instead of
//! hanging it.
//!
//! Why this suite lives beside the router: the files the mount shows are Amazon EFS's own and
//! the subtree it is rooted at is its access point's, the network the task is placed in and
//! the mount target are Amazon EC2's and Amazon EFS's, the execution role the `awslogs` driver
//! runs under is IAM's, and what the containers wrote is read back with CloudWatch Logs' own
//! `GetLogEvents` — five services agreeing for the rule to hold, every resource created
//! through the real operation of the service that owns it on ONE shared engine.
//!
//! Why it is a suite of its own rather than a row of `all/ecs_efs_volume_mounts.rs`, which
//! drives the same four arms from a revision per arm: that suite's harness — its callers, its
//! availability polling and its revision builder — is private to it and builds a revision that
//! CARRIES the program, which is the opposite of what is under test here; sharing it would mean
//! making the whole of it `pub(crate)` and giving its revision builder an "and no program"
//! mode. The two suites are to be folded together, harness lifted into one place, by the
//! issue that next touches either.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

const REGION: &str = "us-west-2";
const CLUSTER: &str = "efshandover";
const CONTAINER: &str = "app";
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";
const LOG_GROUP: &str = "/emulator/ecs/efs-handover";
const STREAM_PREFIX: &str = "efs";
const MARKER: &str = "handover-marker-0123456789";
const READ_ONLY_FILE_SYSTEM: &str = "Read-only file system";
const DEADLINE: Duration = Duration::from_secs(120);
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(100);

fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_efs_magnolioapiservice_v20150201::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    (router, engine)
}

fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("the test body serializes"),
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

async fn ecs_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    let (status, answer) = rpc(
        router,
        "ecs",
        "AmazonEC2ContainerServiceV20141113",
        operation,
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ecs {operation} must answer 200, got {status} with {answer}"
    );
    answer
}

async fn logs(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    rpc(router, "logs", "Logs_20140328", operation, body).await
}

async fn efs(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!(
            "https://elasticfilesystem.{REGION}.amazonaws.com{path}"
        ))
        .header("host", format!("elasticfilesystem.{REGION}.amazonaws.com"))
        .header("authorization", authorization("elasticfilesystem"));
    let bytes = match &body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            Bytes::from(serde_json::to_vec(value).expect("the test body serializes"))
        }
        None => Bytes::new(),
    };
    let request = builder.body(bytes).expect("a well-formed request");
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

async fn ec2_element(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    parameters: &[(&str, &str)],
    element: &str,
) -> String {
    let mut body = format!("Action={action}&Version=2016-11-15");
    for (name, value) in parameters {
        body.push('&');
        body.push_str(&crate::query_wire::form_encode(name));
        body.push('=');
        body.push_str(&crate::query_wire::form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ec2.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("ec2"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: ec2 {action} must answer 200, got {status} with {text}"
    );
    crate::query_wire::opt_element(&text, element)
        .unwrap_or_else(|| panic!("case {case:?}: no <{element}> in {text}"))
}

async fn iam(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    parameters: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in parameters {
        body.push('&');
        body.push_str(&crate::query_wire::form_encode(name));
        body.push('=');
        body.push_str(&crate::query_wire::form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("iam"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: iam {action} must answer 200, got {status} with {text}"
    );
    text
}

/// The execution role a Fargate task delivering through `awslogs` needs.
async fn execution_role(router: &MultiServiceRouter, case: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "ecs-tasks.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = iam(
        router,
        case,
        "CreateRole",
        &[
            ("RoleName", "efshandoverexec"),
            ("AssumeRolePolicyDocument", &trust),
        ],
    )
    .await;
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["logs:CreateLogStream", "logs:PutLogEvents"],
            "Resource": "*",
        }],
    })
    .to_string();
    iam(
        router,
        case,
        "PutRolePolicy",
        &[
            ("RoleName", "efshandoverexec"),
            ("PolicyName", "deliver-container-logs"),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    crate::query_wire::opt_element(&created, "Arn")
        .unwrap_or_else(|| panic!("case {case:?}: no <Arn> in {created}"))
}

fn stream_of(task_arn: &str) -> String {
    let task_id = task_arn.rsplit('/').next().unwrap_or_default();
    format!("{STREAM_PREFIX}/{CONTAINER}/{task_id}")
}

/// Look again until `task_arn` reports STOPPED, then answer its record and the lines its
/// container delivered.
async fn ran(
    router: &MultiServiceRouter,
    task_arn: &str,
    case: &str,
) -> (Value, Vec<String>) {
    let deadline = Instant::now() + DEADLINE;
    let task = loop {
        let body = ecs_ok(
            router,
            case,
            "DescribeTasks",
            json!({ "cluster": CLUSTER, "tasks": [task_arn] }),
        )
        .await;
        let task = body["tasks"][0].clone();
        if task["lastStatus"] == json!("STOPPED") {
            break task;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the task never stopped, last read {task}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    };
    let (status, read) = logs(
        router,
        "GetLogEvents",
        json!({
            "logGroupName": LOG_GROUP,
            "logStreamName": stream_of(task_arn),
            "startFromHead": true,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetLogEvents must answer 200, got {status} with {read}"
    );
    let lines: Vec<String> = read["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|event| event["message"].as_str().map(str::to_string))
        .collect();
    (task, lines)
}

fn reported(lines: &[String], name: &str) -> Option<String> {
    lines
        .iter()
        .find_map(|line| line.strip_prefix(&format!("{name}=")))
        .map(str::to_string)
}

#[tokio::test]
async fn the_recorded_shared_file_system_handover() {
    let (router, engine) = emulator();
    let case = "handover";

    let vpc = ec2_element(
        &router,
        case,
        "CreateVpc",
        &[("CidrBlock", "10.92.0.0/16")],
        "vpcId",
    )
    .await;
    let subnet = ec2_element(
        &router,
        case,
        "CreateSubnet",
        &[
            ("VpcId", vpc.as_str()),
            ("CidrBlock", "10.92.0.0/24"),
            ("AvailabilityZone", &format!("{REGION}a")),
        ],
        "subnetId",
    )
    .await;
    crate::ec2_network::register_internet_route(&engine, &vpc);
    let role_arn = execution_role(&router, case).await;
    ecs_ok(
        &router,
        case,
        "CreateCluster",
        json!({ "clusterName": CLUSTER }),
    )
    .await;
    let (status, created) = logs(
        &router,
        "CreateLogGroup",
        json!({ "logGroupName": LOG_GROUP }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateLogGroup must answer 200, got {status} with {created}"
    );

    // The file system, its mount target in the task's subnet and an access point rooted at
    // /data, each polled to `available` the way a client does.
    let (status, file_system) = efs(
        &router,
        "POST",
        "/2015-02-01/file-systems",
        Some(json!({ "CreationToken": "handover" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateFileSystem must answer 201, got {status} with {file_system}"
    );
    let file_system_id = file_system["FileSystemId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no FileSystemId in {file_system}"))
        .to_string();
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (_, read) = efs(
            &router,
            "GET",
            &format!("/2015-02-01/file-systems?FileSystemId={file_system_id}"),
            None,
        )
        .await;
        if read["FileSystems"][0]["LifeCycleState"].as_str() == Some("available") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the file system never reached available, last read {read}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
    let (status, mount_target) = efs(
        &router,
        "POST",
        "/2015-02-01/mount-targets",
        Some(json!({ "FileSystemId": file_system_id, "SubnetId": subnet })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateMountTarget must answer 200, got {status} with {mount_target}"
    );
    let (status, access_point) = efs(
        &router,
        "POST",
        "/2015-02-01/access-points",
        Some(json!({
            "FileSystemId": file_system_id,
            "ClientToken": "handover-access-point",
            "RootDirectory": {
                "Path": "/data",
                "CreationInfo": { "OwnerUid": 0, "OwnerGid": 0, "Permissions": "0755" },
            },
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAccessPoint must answer 200, got {status} with {access_point}"
    );
    let access_point_id = access_point["AccessPointId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no AccessPointId in {access_point}"))
        .to_string();
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (_, read) = efs(
            &router,
            "GET",
            &format!("/2015-02-01/access-points?AccessPointId={access_point_id}"),
            None,
        )
        .await;
        if read["AccessPoints"][0]["LifeCycleState"].as_str() == Some("available") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the access point never reached available, last read {read}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }

    // TWO revisions over that one file system — one mounting it read-write, one read-only —
    // and no program of their own: every run below sends its program as a container override.
    for (family, read_only) in [("handoverrw", false), ("handoverro", true)] {
        ecs_ok(
            &router,
            case,
            "RegisterTaskDefinition",
            json!({
                "family": family,
                "networkMode": "awsvpc",
                "cpu": "256",
                "memory": "512",
                "executionRoleArn": role_arn,
                "requiresCompatibilities": ["FARGATE"],
                "volumes": [{
                    "name": "shared",
                    "efsVolumeConfiguration": {
                        "fileSystemId": file_system_id,
                        "transitEncryption": "ENABLED",
                        "authorizationConfig": {
                            "accessPointId": access_point_id,
                            "iam": "ENABLED",
                        },
                    },
                }],
                "containerDefinitions": [{
                    "name": CONTAINER,
                    "image": IMAGE,
                    "essential": true,
                    "mountPoints": [{
                        "sourceVolume": "shared",
                        "containerPath": "/mnt/shared",
                        "readOnly": read_only,
                    }],
                    "logConfiguration": {
                        "logDriver": "awslogs",
                        "options": {
                            "awslogs-group": LOG_GROUP,
                            "awslogs-region": REGION,
                            "awslogs-stream-prefix": STREAM_PREFIX,
                        },
                    },
                }],
            }),
        )
        .await;
    }

    // The four runs, each started only once the one before it has stopped, each program sent
    // as this launch's own override of the revision it reuses.
    let arms: [(&str, &str, &str); 4] = [
        (
            "handoverrw",
            "A",
            "set -e; echo \"$MARKER\" > /mnt/shared/payload; sync; \
             echo \"A-SIZE=$(wc -c < /mnt/shared/payload)\"; \
             echo \"A-MODE=$(ls -l /mnt/shared/payload | cut -c1-10)\"; echo A-DONE",
        ),
        (
            "handoverrw",
            "B",
            "echo \"B-SAW=$(cat /mnt/shared/payload)\"; \
             echo B-APPEND >> /mnt/shared/payload; echo B-DONE",
        ),
        (
            "handoverro",
            "C",
            "echo \"C-SAW=$(cat /mnt/shared/payload)\"; \
             echo C-APPEND >> /mnt/shared/payload 2>&1 && echo C-WROTE; echo C-DONE",
        ),
        (
            "handoverrw",
            "D",
            "echo \"D-SAW=$(cat /mnt/shared/payload)\"; echo D-DONE",
        ),
    ];
    let mut delivered: Vec<Vec<String>> = Vec::new();
    for (family, arm, program) in arms {
        let body = ecs_ok(
            &router,
            arm,
            "RunTask",
            json!({
                "cluster": CLUSTER,
                "taskDefinition": family,
                "count": 1,
                "launchType": "FARGATE",
                "networkConfiguration": {
                    "awsvpcConfiguration": {
                        "subnets": [subnet.clone()],
                        "assignPublicIp": "ENABLED",
                    }
                },
                "overrides": {
                    "containerOverrides": [{
                        "name": CONTAINER,
                        "command": ["sh", "-c", program],
                        "environment": [{ "name": "MARKER", "value": MARKER }],
                    }],
                },
            }),
        )
        .await;
        let task_arn = body["tasks"][0]["taskArn"]
            .as_str()
            .unwrap_or_else(|| panic!("arm {arm:?}: RunTask answers a task ARN, got {body}"))
            .to_string();
        let (task, lines) = ran(&router, &task_arn, arm).await;
        assert_ne!(
            task["stopCode"],
            json!("TaskFailedToStart"),
            "arm {arm:?}: the run must have STARTED, got {task}"
        );
        assert_eq!(
            task["containers"][0]["exitCode"],
            json!(0),
            "arm {arm:?}: expected the program's own status, got {task}"
        );
        delivered.push(lines);
    }

    let writer = &delivered[0];
    let expected_size = (MARKER.len() + 1).to_string();
    assert_eq!(
        reported(writer, "A-SIZE").as_deref(),
        Some(expected_size.as_str()),
        "arm A: expected A-SIZE={expected_size}, got {writer:?}"
    );
    let reader = &delivered[1];
    assert_eq!(
        reported(reader, "B-SAW").as_deref(),
        Some(MARKER),
        "arm B: expected the second run to read the first run's bytes, got {reader:?}"
    );
    let refused = &delivered[2];
    assert_eq!(
        reported(refused, "C-SAW").as_deref(),
        Some(MARKER),
        "arm C: expected the read through the read-only mount to answer, got {refused:?}"
    );
    assert!(
        refused
            .iter()
            .any(|line| line.contains(READ_ONLY_FILE_SYSTEM)),
        "arm C: expected the refused write to be reported with {READ_ONLY_FILE_SYSTEM:?}, got \
         {refused:?}"
    );
    assert!(
        !refused.iter().any(|line| line == "C-WROTE"),
        "arm C: a write the mount refused must not be reported as done, got {refused:?}"
    );
    let checked = &delivered[3];
    assert_eq!(
        reported(checked, "D-SAW").as_deref(),
        Some(MARKER),
        "arm D: expected the volume to still hold the writer's bytes, got {checked:?}"
    );
    // What D read is the whole file, which the read-write run appended a line to: the
    // substitution keeps the line the second run added, and the read-only run's line is
    // nowhere, so the refusal left the volume as it was.
    assert!(
        checked.iter().any(|line| line == "B-APPEND"),
        "arm D: expected the volume to hold what the read-write run appended, got {checked:?}"
    );
    assert!(
        !checked.iter().any(|line| line.contains("C-APPEND")),
        "arm D: the refused write must have left no trace on the volume, got {checked:?}"
    );
}
