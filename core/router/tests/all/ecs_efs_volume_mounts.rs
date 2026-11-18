// SPDX-License-Identifier: CC0-1.0

//! A task-definition VOLUME is a filesystem inside the container that mounts it, across the
//! services the mount spans.
//!
//! RULE under test: a volume a task definition declares is mounted into every container that
//! lists it in `containerDefinitions[].mountPoints`, at that entry's `containerPath`, as a
//! real POSIX filesystem — a redirection into it creates a file, the utilities report the
//! bytes and the mode of what was written, and a read gets those bytes back. WHICH files the
//! mount shows is the volume's own namespace, never a copy: an `efsVolumeConfiguration`
//! volume's files are the named Amazon EFS file system's, rooted at the path its access point
//! enforces, so a container of a LATER task reads what an earlier task's container wrote,
//! while a volume whose data belongs to the task is a namespace of that task alone. A mount
//! point whose `readOnly` is `true` makes the container's access read-only: the kernel refuses
//! every write to it with EROFS, whose text is `Read-only file system`, while a read through
//! the same mount still succeeds — and the refused write leaves the volume as it was.
//!
//! Why this suite lives beside the router: the FILES a container sees at an
//! `efsVolumeConfiguration` mount point are Amazon EFS's own, held beside the file system
//! record that names them, and the subtree and identity the mount gets are the access point's
//! — so Amazon ECS, Amazon EFS and CloudWatch Logs have to agree for the rule to hold. Every
//! resource below is created through the real operation of the service that owns it on ONE
//! shared engine, and everything asserted is read back through a real read: the file system
//! and its access point through Amazon EFS, the task and its `exitCode` through
//! `DescribeTasks`, and what the container WROTE through CloudWatch Logs' own `GetLogEvents`
//! — never out of a producer's own buffer.
//!
//! Member and doc source: `smithy_query(query="shape", shape="MountPoint")` —
//! `sourceVolume` "the name of the volume to mount", `containerPath` "the path on the
//! container to mount the host volume at", `readOnly` "if this value is `true`, the container
//! has read-only access to the volume" — and `smithy_query(query="shape",
//! shape="EFSVolumeConfiguration")`, whose `rootDirectory` "must either be omitted or set to
//! `/` which will enforce the path set on the EFS access point".

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The cluster every task below runs in.
const CLUSTER: &str = "efsmounts";

/// The one container of every task definition below.
const CONTAINER: &str = "app";

/// The image the containers start from. Nothing is fetched from it: what runs is the
/// `command` the revision states.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// The log group every container delivers its output to, created through CloudWatch Logs'
/// own API before any task is launched.
const LOG_GROUP: &str = "/emulator/ecs/efs-mounts";

/// The `awslogs-stream-prefix` every task definition configures, and so the first segment of
/// every stream name they deliver to.
const STREAM_PREFIX: &str = "mount";

/// The EC2 instance the cluster's capacity is registered as.
const INSTANCE_ID: &str = "i-0c0ffee0c0ffee0c0";

/// The account id every emulator ARN carries, which a container-instance registration's
/// identity document has to name.
const ACCOUNT: &str = "000000000000";

/// The text the kernel's EROFS is reported with, which a write through a read-only mount is
/// refused with. Spelled out here rather than imported, so a wrong wording in the
/// implementation cannot make a test pass.
const READ_ONLY_FILE_SYSTEM: &str = "Read-only file system";

/// How long a poll keeps looking before it calls a clock-owed move stalled, and how long it
/// leaves between looks. Generous on purpose: the wait ends on the CONDITION, and the deadline
/// is only there so a move that never happens fails the test instead of hanging it. The
/// interval is long enough that looking again costs the emulator little while the suite runs
/// beside every other test of the workspace.
const DEADLINE: Duration = Duration::from_secs(120);
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(150);

/// One emulator carrying the three services a mount spans over ONE shared engine, as the
/// production router mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
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
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(engine),
    );
    router
}

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

/// A string member of a body, which has to be there.
fn string(body: &Value, member: &str, case: &str) -> String {
    body[member]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a string {member}, got {body}"))
        .to_string()
}

/// Create one Amazon EFS file system through that service's own API and wait for the
/// clock-owed `creating` -> `available` move, which is what a client does before it mounts
/// one. Answers its `fs-…` id.
async fn available_file_system(router: &MultiServiceRouter, token: &str, case: &str) -> String {
    let (status, created) = efs(
        router,
        "POST",
        "/2015-02-01/file-systems",
        Some(json!({ "CreationToken": token })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateFileSystem must answer 201, got {status} with {created}"
    );
    let id = string(&created, "FileSystemId", case);
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (_, read) = efs(
            router,
            "GET",
            &format!("/2015-02-01/file-systems?FileSystemId={id}"),
            None,
        )
        .await;
        if read["FileSystems"][0]["LifeCycleState"].as_str() == Some("available") {
            return id;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: file system {id} never reached available, last read {read}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// Create one access point on `file_system_id`, rooted at `path` and applying `posix_user`
/// when one is given, and wait for it to report `available`. Answers its `fsap-…` id.
async fn available_access_point(
    router: &MultiServiceRouter,
    file_system_id: &str,
    path: &str,
    posix_user: Option<(i64, i64)>,
    case: &str,
) -> String {
    let mut body = json!({
        "FileSystemId": file_system_id,
        "ClientToken": format!("token-{file_system_id}-{path}"),
        "RootDirectory": {
            "Path": path,
            "CreationInfo": { "OwnerUid": 0, "OwnerGid": 0, "Permissions": "0755" },
        },
    });
    if let Some((uid, gid)) = posix_user {
        body["PosixUser"] = json!({ "Uid": uid, "Gid": gid });
    }
    let (status, created) = efs(router, "POST", "/2015-02-01/access-points", Some(body)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAccessPoint must answer 200, got {status} with {created}"
    );
    let id = string(&created, "AccessPointId", case);
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (_, read) = efs(
            router,
            "GET",
            &format!("/2015-02-01/access-points?AccessPointId={id}"),
            None,
        )
        .await;
        if read["AccessPoints"][0]["LifeCycleState"].as_str() == Some("available") {
            return id;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: access point {id} never reached available, last read {read}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// The cluster every task runs in, with ONE container instance registered into it through the
/// agent's own operation so an `EC2` launch has somewhere to run, and the log group the
/// containers deliver to.
async fn cluster_and_log_group(router: &MultiServiceRouter, case: &str) {
    ecs_ok(router, case, "CreateCluster", json!({ "clusterName": CLUSTER })).await;
    let identity = json!({
        "accountId": ACCOUNT,
        "instanceId": INSTANCE_ID,
        "region": REGION,
        "instanceType": "m5.large",
        "availabilityZone": format!("{REGION}a"),
    })
    .to_string();
    ecs_ok(
        router,
        case,
        "RegisterContainerInstance",
        json!({
            "cluster": CLUSTER,
            "instanceIdentityDocument": identity,
            // A registration presents the document's signature beside it; nothing here signs
            // anything, so this is base64 text of the right shape.
            "instanceIdentityDocumentSignature": "ZXhhbXBsZQ".repeat(35),
        }),
    )
    .await;
    let (status, created) = logs(
        router,
        "CreateLogGroup",
        json!({ "logGroupName": LOG_GROUP }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateLogGroup must answer 200, got {status} with {created}"
    );
}

/// Register one revision whose single container runs `program` with `volumes` declared and
/// `mount_points` mounted into it, and answer its ARN.
async fn register(
    router: &MultiServiceRouter,
    family: &str,
    volumes: Value,
    mount_points: Value,
    program: &str,
    environment: &[(&str, &str)],
    case: &str,
) -> String {
    let body = ecs_ok(
        router,
        case,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "volumes": volumes,
            "containerDefinitions": [{
                "name": CONTAINER,
                "image": IMAGE,
                "memory": 128,
                "essential": true,
                "command": ["sh", "-c", program],
                "mountPoints": mount_points,
                "environment": environment
                    .iter()
                    .map(|(name, value)| json!({ "name": name, "value": value }))
                    .collect::<Vec<Value>>(),
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
    string(&body["taskDefinition"], "taskDefinitionArn", case)
}

/// An `efsVolumeConfiguration` volume named `name` over `file_system_id`, through
/// `access_point_id` when one is given.
fn efs_volume(name: &str, file_system_id: &str, access_point_id: Option<&str>) -> Value {
    let mut configuration = json!({
        "fileSystemId": file_system_id,
        "transitEncryption": "ENABLED",
    });
    if let Some(access_point_id) = access_point_id {
        configuration["authorizationConfig"] =
            json!({ "accessPointId": access_point_id, "iam": "ENABLED" });
    }
    json!([{ "name": name, "efsVolumeConfiguration": configuration }])
}

/// One `mountPoints` entry: `name` at `container_path`, read-only when `read_only`.
fn mount_point(name: &str, container_path: &str, read_only: bool) -> Value {
    json!([{
        "sourceVolume": name,
        "containerPath": container_path,
        "readOnly": read_only,
    }])
}

/// `RunTask` one standalone task of `family` and answer its ARN.
async fn run_task(router: &MultiServiceRouter, family: &str, case: &str) -> String {
    let body = ecs_ok(
        router,
        case,
        "RunTask",
        json!({ "cluster": CLUSTER, "taskDefinition": family, "launchType": "EC2" }),
    )
    .await;
    let tasks = body["tasks"].as_array().cloned().unwrap_or_default();
    let task = tasks
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: expected RunTask to place one task, got {body}"));
    string(task, "taskArn", case)
}

/// The stream the `awslogs` driver delivers one container of one task to:
/// `<prefix>/<container name>/<task id>`, the naming that driver's own documentation states.
fn stream_of(task_arn: &str) -> String {
    let task_id = task_arn.rsplit('/').next().unwrap_or_default();
    format!("{STREAM_PREFIX}/{CONTAINER}/{task_id}")
}

/// Look again until the task `task_arn` reports `STOPPED`, then answer the lines its container
/// delivered to its own `awslogs` stream, in the order they were written.
///
/// The CONDITION is polled rather than a span slept for: the task's own end is what makes its
/// container's run certainly over, so the lines read afterwards are the whole of what the
/// program wrote.
async fn delivered_lines(
    router: &MultiServiceRouter,
    task_arn: &str,
    case: &str,
) -> (Vec<String>, Value) {
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
        if task["lastStatus"].as_str() == Some("STOPPED") {
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
    let lines = read["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|event| event["message"].as_str().map(str::to_string))
        .collect();
    (lines, task)
}

/// The value a line `NAME=<value>` carries, and `None` when no line names it.
fn reported(lines: &[String], name: &str) -> Option<String> {
    lines
        .iter()
        .find_map(|line| line.strip_prefix(&format!("{name}=")))
        .map(str::to_string)
}

/// RULE, over the whole scenario the issue's recording drives on the FARGATE launch path: a
/// Fargate task whose task definition declares an `efsVolumeConfiguration` volume has that
/// file system mounted at its container's `containerPath`, so the container's own utilities
/// report the file it wrote there; a LATER task mounting the same file system reads those
/// exact bytes and appends to them; a task whose mount point is `readOnly` still reads the
/// current content but has its write refused with the kernel's `Read-only file system` and
/// never reports the write as done; and a fourth task confirms the refused write left no
/// trace.
///
/// The programs are the ones such a workload writes — the `set -e` they begin with, the
/// redirections, the command substitutions and the pipeline included — and every line asserted
/// is read back out of CloudWatch Logs.
#[tokio::test]
async fn the_shared_file_system_scenario_runs_on_the_fargate_launch_path() {
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
    let case = "the shared file system scenario";

    // The network the task is placed in, created through Amazon EC2's OWN API: the subnet a
    // placement names is that service's resource, and both Amazon EFS (for the mount target
    // it places) and Amazon ECS (for the interface it attaches) resolve it there.
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
    // A Fargate task with a public address fetches its image over the VPC's own route off it,
    // which Amazon ECS reads before it starts the task.
    crate::ec2_network::register_internet_route(&engine, &vpc);

    let role_arn = execution_role(&router, case).await;
    ecs_ok(&router, case, "CreateCluster", json!({ "clusterName": CLUSTER })).await;
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
    let file_system = available_file_system(&router, "fargate-scenario", case).await;
    // The mount target a task's networking reaches the file system through, in the subnet the
    // placement names.
    let (mount_status, mount_target) = efs(
        &router,
        "POST",
        "/2015-02-01/mount-targets",
        Some(json!({ "FileSystemId": file_system, "SubnetId": subnet })),
    )
    .await;
    assert_eq!(
        mount_status, 200,
        "case {case:?}: CreateMountTarget must answer 200, got {mount_status} with {mount_target}"
    );
    let access_point = available_access_point(&router, &file_system, "/data", None, case).await;
    let marker = "scenario-marker-0123456789";

    // (the family, whether its mount point is read-only, the program its container runs)
    let arms = [
        (
            "scenariowriter",
            false,
            "set -e; echo \"$MARKER\" > /mnt/shared/payload; sync; \
             echo \"A-SIZE=$(wc -c < /mnt/shared/payload)\"; \
             echo \"A-MODE=$(ls -l /mnt/shared/payload | cut -c1-10)\"; echo A-DONE",
        ),
        (
            "scenarioreader",
            false,
            "echo \"B-SAW=$(cat /mnt/shared/payload)\"; \
             echo B-APPEND >> /mnt/shared/payload; echo B-DONE",
        ),
        (
            "scenarioreadonly",
            true,
            "echo \"C-SAW=$(cat /mnt/shared/payload)\"; \
             echo C-APPEND >> /mnt/shared/payload 2>&1 && echo C-WROTE; echo C-DONE",
        ),
        (
            "scenariocheck",
            false,
            "echo \"D-SAW=$(cat /mnt/shared/payload)\"; echo D-DONE",
        ),
    ];
    for (family, read_only, program) in arms {
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
                "volumes": efs_volume("shared", &file_system, Some(&access_point)),
                "containerDefinitions": [{
                    "name": CONTAINER,
                    "image": IMAGE,
                    "essential": true,
                    "command": ["sh", "-c", program],
                    "mountPoints": mount_point("shared", "/mnt/shared", read_only),
                    "environment": [{ "name": "MARKER", "value": marker }],
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

    // Each arm runs only once the one before it has STOPPED, which is what makes the
    // hand-over a hand-over rather than two containers writing at once.
    let mut delivered: Vec<Vec<String>> = Vec::new();
    for (family, _, _) in arms {
        let body = ecs_ok(
            &router,
            case,
            "RunTask",
            json!({
                "cluster": CLUSTER,
                "taskDefinition": family,
                "launchType": "FARGATE",
                "networkConfiguration": {
                    "awsvpcConfiguration": {
                        "subnets": [subnet],
                        "assignPublicIp": "ENABLED",
                    }
                },
            }),
        )
        .await;
        let arn = string(&body["tasks"][0], "taskArn", case);
        let (lines, _) = delivered_lines(&router, &arn, case).await;
        delivered.push(lines);
    }

    // The WRITER: the file it created holds the marker and the newline `echo` ended it with,
    // and a long listing reports a regular writable file.
    let writer = &delivered[0];
    let expected_size = (marker.len() + 1).to_string();
    let size = reported(writer, "A-SIZE");
    assert_eq!(
        size.as_deref(),
        Some(expected_size.as_str()),
        "expected A-SIZE={expected_size} from the container's own wc, got {size:?} in {writer:?}"
    );
    let mode = reported(writer, "A-MODE");
    assert!(
        mode.as_deref().is_some_and(|mode| mode.starts_with("-rw")),
        "expected A-MODE to report a regular writable file, got {mode:?} in {writer:?}"
    );
    assert!(
        writer.iter().any(|line| line == "A-DONE"),
        "expected the writing program to run to its end, got {writer:?}"
    );

    // The READER of the next task: the first task's exact bytes.
    let reader = &delivered[1];
    let saw = reported(reader, "B-SAW");
    assert_eq!(
        saw.as_deref(),
        Some(marker),
        "expected the second task to read the first task's bytes, got {saw:?} in {reader:?}"
    );
    assert!(
        reader.iter().any(|line| line == "B-DONE"),
        "expected the reading program to run to its end, got {reader:?}"
    );

    // The READ-ONLY arm: the read answers, the write is refused with EROFS, and the write is
    // never reported as done.
    let refused = &delivered[2];
    assert_eq!(
        reported(refused, "C-SAW").as_deref(),
        Some(marker),
        "expected the read through the read-only mount to answer the volume's bytes, got \
         {refused:?}"
    );
    assert!(
        refused
            .iter()
            .any(|line| line.contains(READ_ONLY_FILE_SYSTEM)),
        "expected the write through the read-only mount to be refused with \
         {READ_ONLY_FILE_SYSTEM:?}, got {refused:?}"
    );
    assert!(
        !refused.iter().any(|line| line == "C-WROTE"),
        "the refused write must not be reported as done, got {refused:?}"
    );
    assert!(
        refused.iter().any(|line| line == "C-DONE"),
        "expected the read-only program to run to its end, got {refused:?}"
    );

    // The CONFIRMATION: the volume holds what the writer and the reader left, and nothing of
    // the refused write.
    let checked = &delivered[3];
    assert_eq!(
        reported(checked, "D-SAW").as_deref(),
        Some(marker),
        "expected the volume to still hold the writer's bytes, got {checked:?}"
    );
    assert!(
        checked.iter().any(|line| line == "B-APPEND"),
        "expected the volume to hold what the second task appended, got {checked:?}"
    );
    assert!(
        !checked.iter().any(|line| line == "C-APPEND"),
        "the refused write must have left no trace on the volume, got {checked:?}"
    );
    assert!(
        checked.iter().any(|line| line == "D-DONE"),
        "expected the confirming program to run to its end, got {checked:?}"
    );
}

/// The task EXECUTION role a Fargate task with the `awslogs` driver needs — "Fargate requires
/// task definition to have execution role ARN to support log driver awslogs" — created through
/// IAM's own API, trusted by this service's principal and allowed the log calls the driver
/// makes. Answers its ARN.
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
            ("RoleName", "efsmountexec"),
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
            ("RoleName", "efsmountexec"),
            ("PolicyName", "deliver-container-logs"),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    crate::query_wire::opt_element(&created, "Arn")
        .unwrap_or_else(|| panic!("case {case:?}: no <Arn> in {created}"))
}

/// One ec2Query action that has to succeed, answered with the first `element` of its XML body
/// — the identifier Amazon EC2 minted for the resource the action created.
async fn ec2_element(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    parameters: &[(&str, &str)],
    element: &str,
) -> String {
    // The API version an ec2Query body carries.
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

/// RULE: the volume a task definition declares is a real filesystem at each `containerPath`
/// its containers mount it at — a redirection creates a file there, `wc -c` reports the bytes
/// written, `ls -l` reports a regular writable file, and `cat` reads the bytes back.
///
/// The rows differ along what the rule generalises over: WHICH kind of volume backs the mount
/// (an Amazon EFS file system through an access point, the same file system mounted at its own
/// root, a volume whose data belongs to the task), WHERE it is mounted, the identity the
/// access point applies to the files, and the bytes the program writes (a plain marker, one
/// carrying spaces, one long enough that its own byte count is a two-digit number).
#[tokio::test]
async fn a_declared_volume_is_a_filesystem_at_each_mount_point() {
    let router = emulator();
    cluster_and_log_group(&router, "mounted volumes").await;
    let file_system = available_file_system(&router, "mounted-volumes", "mounted volumes").await;
    let rooted = available_access_point(
        &router,
        &file_system,
        "/data",
        Some((1000, 1000)),
        "mounted volumes",
    )
    .await;

    struct Case {
        name: &'static str,
        volumes: Value,
        container_path: &'static str,
        marker: &'static str,
        /// The user and group a long listing reports the written file as belonging to.
        owner: &'static str,
    }

    let cases = [
        Case {
            name: "an EFS file system through an access point rooted under it",
            volumes: efs_volume("shared", &file_system, Some(&rooted)),
            container_path: "/mnt/shared",
            marker: "handed-over-payload",
            // The access point applies "an operating system user and group ... to any file
            // system request made through the access point", and the container holds no name
            // for that id, so a listing reports the id itself.
            owner: "1000 1000",
        },
        Case {
            name: "the same EFS file system mounted at its own root",
            volumes: efs_volume("whole", &file_system, None),
            container_path: "/efs",
            marker: "a marker with spaces in it",
            owner: "root root",
        },
        Case {
            name: "a volume whose data belongs to the task",
            volumes: json!([{ "name": "scratch", "host": {} }]),
            container_path: "/scratch",
            marker: "a marker long enough to be counted in two digits",
            owner: "root root",
        },
    ];

    for (index, case) in cases.iter().enumerate() {
        let name = case.name;
        let family = format!("mounted{index}");
        let path = format!("{}/payload", case.container_path);
        let program = format!(
            "set -e; echo \"$MARKER\" > {path}; sync; \
             echo \"SIZE=$(wc -c < {path})\"; \
             echo \"MODE=$(ls -l {path} | cut -c1-10)\"; \
             echo \"OWNER=$(ls -l {path} | cut -d' ' -f3-4)\"; \
             echo \"BACK=$(cat {path})\"; \
             echo DONE"
        );
        let mount = mount_point(
            case.volumes[0]["name"].as_str().unwrap_or_default(),
            case.container_path,
            false,
        );
        register(
            &router,
            &family,
            case.volumes.clone(),
            mount,
            &program,
            &[("MARKER", case.marker)],
            name,
        )
        .await;
        let arn = run_task(&router, &family, name).await;
        let (lines, task) = delivered_lines(&router, &arn, name).await;

        // The file the redirection created holds exactly the marker and the newline `echo`
        // ends it with, as the container's own `wc -c` counts it.
        let expected_size = (case.marker.len() + 1).to_string();
        let size = reported(&lines, "SIZE");
        assert_eq!(
            size.as_deref(),
            Some(expected_size.as_str()),
            "case {name:?}: expected SIZE={expected_size}, got {size:?} in {lines:?}"
        );
        let mode = reported(&lines, "MODE");
        assert!(
            mode.as_deref().is_some_and(|mode| mode.starts_with("-rw")),
            "case {name:?}: expected a regular writable file's mode, got {mode:?} in {lines:?}"
        );
        let owner = reported(&lines, "OWNER");
        assert_eq!(
            owner.as_deref(),
            Some(case.owner),
            "case {name:?}: expected OWNER={:?}, got {owner:?} in {lines:?}",
            case.owner
        );
        let back = reported(&lines, "BACK");
        assert_eq!(
            back.as_deref(),
            Some(case.marker),
            "case {name:?}: expected the bytes back from the volume, got {back:?} in {lines:?}"
        );
        assert!(
            lines.iter().any(|line| line == "DONE"),
            "case {name:?}: expected the program to run to its end, got {lines:?}"
        );
        assert_eq!(
            task["containers"][0]["exitCode"].as_i64(),
            Some(0),
            "case {name:?}: expected the container to exit 0, got {task}"
        );
    }
}

/// RULE: which files a mount shows is the VOLUME's own namespace. An Amazon EFS volume's are
/// the named file system's, so a container of a later task reads what an earlier task's
/// container wrote; a volume whose data belongs to the task is a namespace of that task alone,
/// and a mount of a DIFFERENT file system is a different namespace again.
#[tokio::test]
async fn what_a_mount_shows_is_the_volumes_own_namespace() {
    let router = emulator();
    cluster_and_log_group(&router, "namespaces").await;
    let file_system = available_file_system(&router, "namespace-one", "namespaces").await;
    let other = available_file_system(&router, "namespace-two", "namespaces").await;
    let access_point =
        available_access_point(&router, &file_system, "/data", None, "namespaces").await;

    // The first task WRITES, through the access point.
    let writer = "namespacewriter";
    register(
        &router,
        writer,
        efs_volume("shared", &file_system, Some(&access_point)),
        mount_point("shared", "/mnt/shared", false),
        "set -e; echo \"$MARKER\" > /mnt/shared/payload; echo WROTE",
        &[("MARKER", "written-by-the-first-task")],
        "namespaces",
    )
    .await;
    let written = run_task(&router, writer, "namespaces").await;
    let (wrote, _) = delivered_lines(&router, &written, "namespaces").await;
    assert!(
        wrote.iter().any(|line| line == "WROTE"),
        "the writing task's program must run to its end, got {wrote:?}"
    );

    // (case, the volumes the reading task declares, what it must read back)
    let rows = [
        (
            "the same file system through the same access point",
            efs_volume("shared", &file_system, Some(&access_point)),
            Some("written-by-the-first-task"),
        ),
        (
            "another file system of the same account",
            efs_volume("shared", &other, Some(
                &available_access_point(&router, &other, "/data", None, "namespaces").await,
            )),
            None,
        ),
        (
            "a volume whose data belongs to the reading task",
            json!([{ "name": "shared", "host": {} }]),
            None,
        ),
    ];

    for (index, (case, volumes, expected)) in rows.into_iter().enumerate() {
        let family = format!("namespacereader{index}");
        register(
            &router,
            &family,
            volumes,
            mount_point("shared", "/mnt/shared", false),
            // `cat` of a path the volume holds nothing at fails, so the line is written with
            // whatever the read answered and the program goes on either way.
            "echo \"SAW=$(cat /mnt/shared/payload)\"; echo READ",
            &[],
            case,
        )
        .await;
        let arn = run_task(&router, &family, case).await;
        let (lines, _) = delivered_lines(&router, &arn, case).await;
        assert!(
            lines.iter().any(|line| line == "READ"),
            "case {case:?}: expected the reading program to run to its end, got {lines:?}"
        );
        let saw = reported(&lines, "SAW");
        match expected {
            Some(marker) => assert_eq!(
                saw.as_deref(),
                Some(marker),
                "case {case:?}: expected the bytes the first task wrote, got {saw:?} in {lines:?}"
            ),
            None => assert_eq!(
                saw.as_deref(),
                Some(""),
                "case {case:?}: expected another namespace to hold nothing at that path, got \
                 {saw:?} in {lines:?}"
            ),
        }
    }
}

/// RULE: a mount point whose `readOnly` is `true` gives the container read-only access to the
/// volume: every write to it is refused with the kernel's EROFS and its `Read-only file
/// system` text, a read through the same mount still succeeds, and the volume is left as it
/// was.
///
/// The rows differ along WHICH write the program makes — a truncating redirection, an
/// appending one, a `touch`, a `mkdir`, a removal — because the mount refuses the write rather
/// than the command, and each of them is a write.
#[tokio::test]
async fn a_read_only_mount_refuses_every_write_and_still_serves_reads() {
    let router = emulator();
    cluster_and_log_group(&router, "read only").await;
    let file_system = available_file_system(&router, "read-only", "read only").await;
    let access_point =
        available_access_point(&router, &file_system, "/data", None, "read only").await;
    let volumes = efs_volume("shared", &file_system, Some(&access_point));
    let marker = "the-bytes-the-writer-left";

    // The read-write task stages the file the read-only tasks read and try to write.
    register(
        &router,
        "readonlywriter",
        volumes.clone(),
        mount_point("shared", "/mnt/shared", false),
        "set -e; echo \"$MARKER\" > /mnt/shared/payload; echo WROTE",
        &[("MARKER", marker)],
        "read only",
    )
    .await;
    let staged = run_task(&router, "readonlywriter", "read only").await;
    let (wrote, _) = delivered_lines(&router, &staged, "read only").await;
    assert!(
        wrote.iter().any(|line| line == "WROTE"),
        "the staging task's program must run to its end, got {wrote:?}"
    );

    // (case, the write the program attempts)
    let rows = [
        ("a truncating redirection", "echo replaced > /mnt/shared/payload"),
        ("an appending redirection", "echo appended >> /mnt/shared/payload"),
        ("a redirection creating another file", "echo other > /mnt/shared/other"),
        ("a touch of the file", "touch /mnt/shared/payload"),
        ("a directory the program makes", "mkdir /mnt/shared/made"),
        ("a removal of the file", "rm /mnt/shared/payload"),
    ];

    for (index, (case, write)) in rows.iter().enumerate() {
        let family = format!("readonly{index}");
        let program = format!(
            "set -e; echo \"SAW=$(cat /mnt/shared/payload)\"; \
             {write} && echo WROTE; \
             echo DONE"
        );
        register(
            &router,
            &family,
            volumes.clone(),
            mount_point("shared", "/mnt/shared", true),
            &program,
            &[],
            case,
        )
        .await;
        let arn = run_task(&router, &family, case).await;
        let (lines, _) = delivered_lines(&router, &arn, case).await;
        // The read through the read-only mount succeeded.
        let saw = reported(&lines, "SAW");
        assert_eq!(
            saw.as_deref(),
            Some(marker),
            "case {case:?}: expected the read through the read-only mount to answer the \
             volume's bytes, got {saw:?} in {lines:?}"
        );
        // The write was refused, with the text the kernel's EROFS is reported with.
        assert!(
            lines
                .iter()
                .any(|line| line.contains(READ_ONLY_FILE_SYSTEM)),
            "case {case:?}: expected a write refused with {READ_ONLY_FILE_SYSTEM:?}, got \
             {lines:?}"
        );
        assert!(
            !lines.iter().any(|line| line == "WROTE"),
            "case {case:?}: the refused write must not be reported as done, got {lines:?}"
        );
        assert!(
            lines.iter().any(|line| line == "DONE"),
            "case {case:?}: expected the program to run to its end, got {lines:?}"
        );
    }

    // The volume is as the staging task left it: nothing a refused write asked for happened.
    register(
        &router,
        "readonlycheck",
        volumes,
        mount_point("shared", "/mnt/shared", false),
        "echo \"SAW=$(cat /mnt/shared/payload)\"; echo \"HELD=$(ls /mnt/shared)\"; echo DONE",
        &[],
        "read only",
    )
    .await;
    let checked = run_task(&router, "readonlycheck", "read only").await;
    let (lines, _) = delivered_lines(&router, &checked, "read only").await;
    let saw = reported(&lines, "SAW");
    assert_eq!(
        saw.as_deref(),
        Some(marker),
        "expected the volume to hold exactly what the writer left, got {saw:?} in {lines:?}"
    );
    let held = reported(&lines, "HELD");
    assert_eq!(
        held.as_deref(),
        Some("payload"),
        "expected the refused writes to have left no trace on the volume, got {held:?} in \
         {lines:?}"
    );
}
