// SPDX-License-Identifier: CC0-1.0

//! The METERED SIZE of the file system an Amazon ECS task wrote to, across the services the
//! scenario spans.
//!
//! RULE under test: `FileSystemSize$Value` is "the latest known metered size (in bytes) of data
//! stored in the file system", so the size Amazon EFS reports for a file system is a metering of
//! the data it HOLDS, whichever client stored it — once a container of a Fargate task has
//! written bytes through an `efsVolumeConfiguration` mount, `DescribeFileSystems` reports those
//! bytes with the `Timestamp` the metering determined them at, and a write the mount REFUSED
//! stores nothing, so it moves that size not at all.
//!
//! Member and doc source: `smithy_query(query="shape", shape="FileSystemSize")`, and
//! `smithy_query(query="shape", shape="MountPoint")` for `readOnly` — "if this value is `true`,
//! the container has read-only access to the volume".
//!
//! Why this suite lives beside the router: the DATA that is metered is written by a container
//! Amazon ECS started, through a mount of an Amazon EFS file system, and read back out of
//! CloudWatch Logs — so the three services have to agree on one file system's contents for the
//! rule to hold. Every resource below is created through the real operation of the service that
//! owns it on ONE shared engine, and every value asserted is read back through a real read.
//!
//! It also drives the SHAPE the recorded scenario takes end to end — the mount target polled to
//! `available`, a task definition whose program arrives as a `RunTask` container override, the
//! task polled to `STOPPED` and its output read out of its `awslogs` stream — so a stage of it
//! that never reached its state would fail here rather than hang a client.
//!
//! Nothing here sleeps for a span: every wait polls its CONDITION until a generous deadline.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The cluster the tasks run in.
const CLUSTER: &str = "efsmetering";

/// The one container of every task definition below.
const CONTAINER: &str = "app";

/// The image the containers start from. Nothing is fetched from it: what runs is the `command`
/// the launch's own container override states.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// The log group the containers deliver their output to.
const LOG_GROUP: &str = "/emulator/ecs/efs-metering";

/// The `awslogs-stream-prefix` the task definitions configure, and so the first segment of the
/// stream name they deliver to.
const STREAM_PREFIX: &str = "efs";

/// Where the file system is mounted inside the containers.
const MOUNT_PATH: &str = "/mnt/shared";

/// The text the kernel's EROFS is reported with, which a write through a read-only mount is
/// refused with. Spelled out here rather than imported, so a wrong wording in the
/// implementation cannot make this test pass.
const READ_ONLY_FILE_SYSTEM: &str = "Read-only file system";

/// How long a poll keeps looking before it calls a state the emulator owes unreached, and how
/// long it leaves between looks. Generous on purpose: the wait ends on the CONDITION, and the
/// deadline is only there so a state that is never reached fails this test instead of hanging
/// it.
const DEADLINE: Duration = Duration::from_secs(180);
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(150);

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request; the signature is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One awsJson1_1 call to the service `signing_name` serves, answered with the HTTP status and
/// the parsed body.
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

/// One Amazon ECS call that has to succeed.
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

/// One CloudWatch Logs call, answered with the HTTP status and the parsed body.
async fn logs(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    rpc(router, "logs", "Logs_20140328", operation, body).await
}

/// One Amazon EFS call — restJson1, so a METHOD and a PATH rather than a target header.
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

/// One ec2Query action that has to succeed, answered with the first `element` of its XML body.
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

/// One awsQuery call to IAM that has to succeed, answered with its XML body.
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

/// One role of this account, trusted by the principal Amazon ECS assumes a task's roles as, and
/// allowed every call the agent makes for the task. Answers its ARN.
async fn task_role(router: &MultiServiceRouter, case: &str, name: &str) -> String {
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
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": [
                "logs:CreateLogStream",
                "logs:PutLogEvents",
                "elasticfilesystem:ClientMount",
                "elasticfilesystem:ClientWrite",
            ],
            "Resource": "*",
        }],
    })
    .to_string();
    iam(
        router,
        case,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "run-the-task"),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    crate::query_wire::opt_element(&created, "Arn")
        .unwrap_or_else(|| panic!("case {case:?}: no <Arn> in {created}"))
}

/// The stream the `awslogs` driver delivers one container of one task to:
/// `<prefix>/<container name>/<task id>`, the naming that driver's own documentation states.
fn stream_of(task_arn: &str) -> String {
    let task_id = task_arn.rsplit('/').next().unwrap_or_default();
    format!("{STREAM_PREFIX}/{CONTAINER}/{task_id}")
}

/// The value a line `NAME=<value>` carries, and `None` when no line names it.
fn reported(lines: &[String], name: &str) -> Option<String> {
    lines
        .iter()
        .find_map(|line| line.strip_prefix(&format!("{name}=")))
        .map(str::to_string)
}

/// The `SizeInBytes` Amazon EFS reports for `file_system_id`, read through its own route.
async fn reported_size(router: &MultiServiceRouter, file_system_id: &str, case: &str) -> Value {
    let (status, read) = efs(
        router,
        "GET",
        &format!("/2015-02-01/file-systems?FileSystemId={file_system_id}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeFileSystems must answer 200, got {status} with {read}"
    );
    read["FileSystems"][0]["SizeInBytes"].clone()
}

/// RULE: the metered size a file system reports is the DATA stored in it, whoever stored it — so
/// it is the bytes a task's container wrote through its mount, and a write a read-only mount
/// refused leaves it where it was.
///
/// The two rows differ along what the rule generalises over: whether the container's write
/// REACHED the file system (a read-write mount, which stores its bytes) or was refused by it (a
/// read-only mount, which stores none). Both arms are driven the way the scenario drives them —
/// the program arriving as a `RunTask` container override — and every value asserted is read
/// back out of CloudWatch Logs, `DescribeTasks` or `DescribeFileSystems`.
#[tokio::test]
async fn the_metered_size_is_the_data_a_task_wrote_through_its_mount() {
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
    let case = "the metered file system";

    // The network the task and the mount target are placed in, created through Amazon EC2's own
    // API: the subnet both services resolve is that service's resource.
    let vpc = ec2_element(
        &router,
        case,
        "CreateVpc",
        &[("CidrBlock", "10.0.0.0/16")],
        "vpcId",
    )
    .await;
    let subnet = ec2_element(
        &router,
        case,
        "CreateSubnet",
        &[
            ("VpcId", vpc.as_str()),
            ("CidrBlock", "10.0.0.0/24"),
            ("AvailabilityZone", &format!("{REGION}a")),
        ],
        "subnetId",
    )
    .await;
    let security_group = ec2_element(
        &router,
        case,
        "CreateSecurityGroup",
        &[
            ("VpcId", vpc.as_str()),
            ("GroupName", "efs-metering-tasks"),
            ("GroupDescription", "the tasks and the mount target"),
        ],
        "groupId",
    )
    .await;
    // A Fargate task with a public address fetches its image over the VPC's own route off it,
    // which Amazon ECS reads before it starts the task.
    crate::ec2_network::register_internet_route(&engine, &vpc);

    let execution_role = task_role(&router, case, "efsmeteringexec").await;
    let task_role_arn = task_role(&router, case, "efsmeteringtask").await;

    // The file system, with the members the scenario creates it with, polled to `available`.
    let (created_status, created) = efs(
        &router,
        "POST",
        "/2015-02-01/file-systems",
        Some(json!({
            "CreationToken": "efs-metering",
            "Encrypted": true,
            "PerformanceMode": "generalPurpose",
            "ThroughputMode": "bursting",
        })),
    )
    .await;
    assert_eq!(
        created_status, 201,
        "case {case:?}: CreateFileSystem must answer 201, got {created_status} with {created}"
    );
    let file_system = created["FileSystemId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no FileSystemId in {created}"))
        .to_string();
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (_, read) = efs(
            &router,
            "GET",
            &format!("/2015-02-01/file-systems?FileSystemId={file_system}"),
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

    // The mount target the task's networking reaches the file system through, polled to
    // `available` — the state the scenario waits for before it launches anything.
    let (mount_status, mount_target) = efs(
        &router,
        "POST",
        "/2015-02-01/mount-targets",
        Some(json!({
            "FileSystemId": file_system,
            "SubnetId": subnet,
            "SecurityGroups": [security_group],
        })),
    )
    .await;
    assert_eq!(
        mount_status, 200,
        "case {case:?}: CreateMountTarget must answer 200, got {mount_status} with {mount_target}"
    );
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (_, read) = efs(
            &router,
            "GET",
            &format!("/2015-02-01/mount-targets?FileSystemId={file_system}"),
            None,
        )
        .await;
        if read["MountTargets"][0]["LifeCycleState"].as_str() == Some("available") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the mount target never reached available, last read {read}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }

    // The access point the mount is made through, polled to `available`.
    let (point_status, point) = efs(
        &router,
        "POST",
        "/2015-02-01/access-points",
        Some(json!({
            "FileSystemId": file_system,
            "ClientToken": "efs-metering-access-point",
            "PosixUser": { "Uid": 1000, "Gid": 1000 },
            "RootDirectory": {
                "Path": "/data",
                "CreationInfo": { "OwnerUid": 1000, "OwnerGid": 1000, "Permissions": "0755" },
            },
        })),
    )
    .await;
    assert_eq!(
        point_status, 200,
        "case {case:?}: CreateAccessPoint must answer 200, got {point_status} with {point}"
    );
    let access_point = point["AccessPointId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no AccessPointId in {point}"))
        .to_string();
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (_, read) = efs(
            &router,
            "GET",
            &format!("/2015-02-01/access-points?AccessPointId={access_point}"),
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

    // A file system nothing has written to holds no data, so it is metered at none.
    let empty = reported_size(&router, &file_system, "nothing written yet").await;
    assert_eq!(
        empty["Value"].as_i64(),
        Some(0),
        "case {:?}: expected a file system nothing has written to to report no stored data, got \
         {empty}",
        "nothing written yet"
    );

    ecs_ok(
        &router,
        case,
        "CreateCluster",
        json!({ "clusterName": CLUSTER }),
    )
    .await;
    let (log_status, log_group) = logs(
        &router,
        "CreateLogGroup",
        json!({ "logGroupName": LOG_GROUP }),
    )
    .await;
    assert_eq!(
        log_status, 200,
        "case {case:?}: CreateLogGroup must answer 200, got {log_status} with {log_group}"
    );

    // The two revisions, identical but for `mountPoints[0].readOnly`, and declaring NO command:
    // the program each container runs arrives with the launch, as a container override.
    for (family, read_only) in [("meteringwriter", false), ("meteringreadonly", true)] {
        ecs_ok(
            &router,
            case,
            "RegisterTaskDefinition",
            json!({
                "family": family,
                "networkMode": "awsvpc",
                "cpu": "256",
                "memory": "512",
                "executionRoleArn": execution_role,
                "taskRoleArn": task_role_arn,
                "requiresCompatibilities": ["FARGATE"],
                "volumes": [{
                    "name": "shared",
                    "efsVolumeConfiguration": {
                        "fileSystemId": file_system,
                        "transitEncryption": "ENABLED",
                        "authorizationConfig": {
                            "accessPointId": access_point,
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
                        "containerPath": MOUNT_PATH,
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

    let marker = "the-bytes-one-task-stored";
    // The bytes the writing container's redirection stores: the marker and the newline `echo`
    // ends it with.
    let stored_bytes = i64::try_from(marker.len() + 1).expect("a marker of a few bytes");

    // (case, the family, the program the launch overrides the container's command with, the
    // sentinel the program's last line writes, the bytes the file system holds afterwards)
    let arms = [
        (
            "a write through a read-write mount",
            "meteringwriter",
            format!(
                "set -e; echo \"$MARKER\" > {MOUNT_PATH}/payload; sync; \
                 echo \"A-SIZE=$(wc -c < {MOUNT_PATH}/payload)\"; echo A-DONE"
            ),
            "A-DONE",
            stored_bytes,
        ),
        (
            "a write refused by a read-only mount",
            "meteringreadonly",
            format!(
                "echo \"C-SAW=$(cat {MOUNT_PATH}/payload)\"; \
                 echo C-APPEND >> {MOUNT_PATH}/payload 2>&1 && echo C-WROTE; echo C-DONE"
            ),
            "C-DONE",
            stored_bytes,
        ),
    ];

    for (arm, family, program, sentinel, expected) in arms {
        let launched = ecs_ok(
            &router,
            arm,
            "RunTask",
            json!({
                "cluster": CLUSTER,
                "taskDefinition": family,
                "launchType": "FARGATE",
                "count": 1,
                "networkConfiguration": {
                    "awsvpcConfiguration": {
                        "subnets": [subnet],
                        "securityGroups": [security_group],
                        "assignPublicIp": "ENABLED",
                    }
                },
                "overrides": {
                    "containerOverrides": [{
                        "name": CONTAINER,
                        "command": ["sh", "-c", program],
                        "environment": [{ "name": "MARKER", "value": marker }],
                    }],
                },
            }),
        )
        .await;
        let task_arn = launched["tasks"][0]["taskArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {arm:?}: expected RunTask to place a task, {launched}"))
            .to_string();

        // The task reaches a terminal, fully attributed state, which is what a client polls for.
        let deadline = Instant::now() + DEADLINE;
        let task = loop {
            let read = ecs_ok(
                &router,
                arm,
                "DescribeTasks",
                json!({ "cluster": CLUSTER, "tasks": [task_arn] }),
            )
            .await;
            let task = read["tasks"][0].clone();
            if task["lastStatus"].as_str() == Some("STOPPED") {
                break task;
            }
            assert!(
                Instant::now() < deadline,
                "case {arm:?}: the task never stopped, last read {task}"
            );
            tokio::time::sleep(LOOK_AGAIN_AFTER).await;
        };
        // "The stop code indicating why a task was stopped": the essential container's program
        // ended, which is not a task that failed to start.
        assert_eq!(
            task["stopCode"].as_str(),
            Some("EssentialContainerExited"),
            "case {arm:?}: expected the stop of a task whose essential container ended, got {task}"
        );
        assert!(
            task["startedAt"].is_number() && task["stoppedAt"].is_number(),
            "case {arm:?}: expected the task to report when it started and stopped, got {task}"
        );
        // "For elastic network interfaces ... includes the network interface ID": the awsvpc
        // task's own attachment.
        let interface = task["attachments"][0]["details"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .find_map(|detail| {
                (detail["name"].as_str() == Some("networkInterfaceId"))
                    .then(|| detail["value"].as_str().unwrap_or_default().to_string())
            });
        assert!(
            interface.is_some_and(|id| id.starts_with("eni-")),
            "case {arm:?}: expected the awsvpc attachment to carry its interface, got {task}"
        );
        assert_eq!(
            (
                task["containers"][0]["lastStatus"].as_str(),
                task["containers"][0]["exitCode"].as_i64(),
            ),
            (Some("STOPPED"), Some(0)),
            "case {arm:?}: expected the container to report that it ran and exited 0, got {task}"
        );

        // What the container WROTE, out of the stream its `awslogs` driver delivered to.
        let (events_status, events) = logs(
            &router,
            "GetLogEvents",
            json!({
                "logGroupName": LOG_GROUP,
                "logStreamName": stream_of(&task_arn),
                "startFromHead": true,
            }),
        )
        .await;
        assert_eq!(
            events_status, 200,
            "case {arm:?}: GetLogEvents must answer 200, got {events_status} with {events}"
        );
        let lines: Vec<String> = events["events"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|event| event["message"].as_str().map(str::to_string))
            .collect();
        assert!(
            lines.iter().any(|line| line == sentinel),
            "case {arm:?}: expected the program to run to its end and deliver {sentinel:?}, got \
             {lines:?}"
        );

        // The metered size is the data the file system now holds.
        let deadline = Instant::now() + DEADLINE;
        let size = loop {
            let size = reported_size(&router, &file_system, arm).await;
            if size["Value"].as_i64() == Some(expected) {
                break size;
            }
            assert!(
                Instant::now() < deadline,
                "case {arm:?}: expected the file system to be metered at {expected} bytes, last \
                 read {size}"
            );
            tokio::time::sleep(LOOK_AGAIN_AFTER).await;
        };
        assert_eq!(
            size["ValueInStandard"].as_i64(),
            Some(expected),
            "case {arm:?}: expected the Standard class to hold all {expected} bytes, got {size}"
        );
        assert!(
            size["Timestamp"].as_f64().is_some_and(|at| at > 0.0),
            "case {arm:?}: expected the metering that determined this size to report when, got \
             {size}"
        );

        // What each arm's own lines say about the write it made.
        match arm {
            "a write through a read-write mount" => {
                let counted = reported(&lines, "A-SIZE");
                assert_eq!(
                    counted.as_deref(),
                    Some(expected.to_string().as_str()),
                    "case {arm:?}: expected the container's own wc to count {expected} bytes, got \
                     {counted:?} in {lines:?}"
                );
            }
            _ => {
                assert_eq!(
                    reported(&lines, "C-SAW").as_deref(),
                    Some(marker),
                    "case {arm:?}: expected the read through the read-only mount to answer the \
                     stored bytes, got {lines:?}"
                );
                assert!(
                    lines.iter().any(|line| line.contains(READ_ONLY_FILE_SYSTEM)),
                    "case {arm:?}: expected the refused write to be reported with \
                     {READ_ONLY_FILE_SYSTEM:?}, got {lines:?}"
                );
                assert!(
                    !lines.iter().any(|line| line == "C-WROTE"),
                    "case {arm:?}: the refused write must not be reported as done, got {lines:?}"
                );
            }
        }
    }
}
