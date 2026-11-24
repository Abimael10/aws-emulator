// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the Amazon EFS FILE SYSTEM a function mounts: `CreateFileSystem`,
//! `CreateMountTarget` and `CreateAccessPoint` on Amazon EFS, `CreateRole`/`PutRolePolicy` on
//! IAM, `CreateVpc`/`CreateSubnet`/`CreateSecurityGroup` on Amazon EC2, then `CreateFunction`
//! with `FileSystemConfigs` and `Invoke`
//! (`POST /2015-03-31/functions/{FunctionName}/invocations`) over the router.
//!
//! The rule under test: `FileSystemConfig$LocalMountPath` is "the path where the function can
//! access the file system, starting with /mnt/"
//! (`smithy_query(query="shape", shape="FileSystemConfig")`), so the access point's own root
//! directory is MOUNTED at that path inside every execution environment of the function — the
//! handler's filesystem calls answer there, the files are the FILE SYSTEM's (so what one
//! function writes, another that mounts the same file system reads, and Amazon EFS itself
//! meters those bytes), the access point confines the function to its `RootDirectory` and the
//! handler's process runs as its `PosixUser`, and a function whose execution role is short of
//! `elasticfilesystem:ClientWrite` has the file system mounted read-only, so a write under the
//! mount point is refused with the errno an NFS client reads for one (`EROFS`).
//!
//! Every row drives real HTTP requests through the production `MultiServiceRouter` with Amazon
//! EFS, IAM and Amazon EC2 mounted beside Lambda on ONE engine: every file system, mount
//! target and access point is one Amazon EFS minted through its own API, every role and policy
//! one IAM persisted, and every VPC, subnet and security group one Amazon EC2 minted — nothing
//! here passes off a value of its own as one of those services'.
//!
//! The rows differ along what the rule generalises over: WHERE the file system is mounted (two
//! `LocalMountPath` values, in two functions), WHICH access point the function mounts (two
//! roots of one file system, one with a `PosixUser` and one without), WHAT the handler does
//! there (a text write read back, bytes, an append, a directory it makes, a listing, a
//! filesystem reading), WHO reads it back (the function that wrote it, another function, and
//! Amazon EFS's own metering of the file system), and WHETHER the function's execution role
//! may write.
//!
//! The suite RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use crate::event_source_mapping_role_access::{allow, given_role, logs_only};
use crate::invoke_request_id::{is_host_bound, one_invocation_stream_at_a_time};
use crate::node_invoke_run::package;
use crate::vpc_attachment_interfaces::{ec2_client, network};

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// Where Amazon EFS binds the three creates this suite drives
/// (`smithy_query(query="operation", operation="CreateFileSystem,CreateMountTarget,CreateAccessPoint", model_path="services/service-com-amazonaws-efs-magnolioapiservice-v20150201/smithy-model/com-amazonaws-efs-magnolioapiservice-v20150201.json")`).
const FILE_SYSTEMS_URI: &str = "/2015-02-01/file-systems";
const MOUNT_TARGETS_URI: &str = "/2015-02-01/mount-targets";
const ACCESS_POINTS_URI: &str = "/2015-02-01/access-points";

/// A SigV4 credential scope naming a signing name in `us-east-1`. Nothing in it is verified:
/// the harness turns request authentication off.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/us-east-1/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=not-verified"
    )
}

/// The `Runtime` every function of this suite is configured with.
const RUNTIME: &str = "python3.13";

/// The `Timeout` every function this suite creates carries, in seconds. Sized the way the
/// sibling suites that RUN handlers size theirs: it has to cover this test binary's own
/// scheduling, not the handler's work, which is immediate.
const FUNCTION_TIMEOUT_SECONDS: i32 = 25;

/// How long a poll loop here waits for a state the CLOCK owes before it reports that it never
/// arrived. Generous: it covers this test binary's own scheduling and never the span under
/// test, because nothing here asserts on how long a transition took.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How often those loops look again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How many times a row re-drives its invocation while the answer reports one of this test
/// binary's own host bounds rather than a run.
const HOST_BOUND_ATTEMPTS: usize = 4;

/// The identity the access point every writing function mounts enforces
/// (`PosixUser$Uid` / `$Gid`).
const ACCESS_POINT_UID: i64 = 1001;
const ACCESS_POINT_GID: i64 = 1002;

/// The user and group a Lambda execution environment runs as when no access point it mounts
/// enforces an identity, which is the owner the environment's own filesystem reports for the
/// files a function writes.
const SANDBOX_UID: i64 = 993;
const SANDBOX_GID: i64 = 990;

/// What a dispatched request answered.
struct Answer {
    status: u16,
    headers: http::HeaderMap,
    body: bytes::Bytes,
}

impl Answer {
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }
}

/// Lambda, Amazon EFS, IAM and Amazon EC2 on ONE engine — exactly what `core/router`
/// assembles.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_efs_magnolioapiservice_v20150201::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    (router, engine)
}

/// Dispatch one request at `host`, signed for `signing_name`.
async fn send(
    router: &MultiServiceRouter,
    signing_name: &str,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> Answer {
    let host = format!("{signing_name}.us-east-1.amazonaws.com");
    let payload = body
        .map(|document| serde_json::to_vec(document).expect("the body serializes"))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(format!("http://{host}{uri}"))
        .header("host", host)
        .header("authorization", authorization(signing_name))
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(payload))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// One Lambda request.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> Answer {
    send(router, "lambda", method, uri, body).await
}

/// One Amazon EFS request.
async fn efs(router: &MultiServiceRouter, method: &str, uri: &str, body: Option<&Value>) -> Answer {
    send(router, "elasticfilesystem", method, uri, body).await
}

/// One `available` Amazon EFS file system, with a mount target in `subnet` so the file system
/// is reachable from inside that VPC, as a client that means to mount it creates them.
async fn available_file_system(
    router: &MultiServiceRouter,
    token: &str,
    subnet_id: &str,
    group_id: &str,
) -> String {
    let created = efs(
        router,
        "POST",
        FILE_SYSTEMS_URI,
        Some(&json!({ "CreationToken": token })),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "CreateFileSystem {token:?} must succeed, got {} {}",
        created.status,
        created.text()
    );
    let file_system_id = created.json()["FileSystemId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateFileSystem must report the id it minted: {}", created.text()))
        .to_string();
    poll_until(
        &format!("file system {file_system_id} is available"),
        || async {
            let read = efs(
                router,
                "GET",
                &format!("{FILE_SYSTEMS_URI}?FileSystemId={file_system_id}"),
                None,
            )
            .await;
            read.json()["FileSystems"][0]["LifeCycleState"].as_str() == Some("available")
        },
    )
    .await;
    let mounted = efs(
        router,
        "POST",
        MOUNT_TARGETS_URI,
        Some(&json!({
            "FileSystemId": file_system_id,
            "SubnetId": subnet_id,
            "SecurityGroups": [group_id],
        })),
    )
    .await;
    assert_eq!(
        mounted.status,
        200,
        "CreateMountTarget in {subnet_id:?} must succeed, got {} {}",
        mounted.status,
        mounted.text()
    );
    let mount_target_id = mounted.json()["MountTargetId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateMountTarget must report the id it minted: {}", mounted.text()))
        .to_string();
    poll_until(
        &format!("mount target {mount_target_id} is available"),
        || async {
            let read = efs(
                router,
                "GET",
                &format!("/2015-02-01/mount-targets?MountTargetId={mount_target_id}"),
                None,
            )
            .await;
            read.json()["MountTargets"][0]["LifeCycleState"].as_str() == Some("available")
        },
    )
    .await;
    file_system_id
}

/// One `available` access point of `file_system_id`, rooted at `root` and enforcing `posix`
/// where the row names one, answering the ARN Amazon EFS minted for it.
async fn available_access_point(
    router: &MultiServiceRouter,
    file_system_id: &str,
    token: &str,
    root: &str,
    posix: Option<(i64, i64)>,
) -> String {
    let mut body = json!({
        "ClientToken": token,
        "FileSystemId": file_system_id,
        "RootDirectory": {
            "Path": root,
            "CreationInfo": { "OwnerUid": 0, "OwnerGid": 0, "Permissions": "0777" },
        },
    });
    if let Some((uid, gid)) = posix {
        body["PosixUser"] = json!({ "Uid": uid, "Gid": gid });
    }
    let created = efs(router, "POST", ACCESS_POINTS_URI, Some(&body)).await;
    assert_eq!(
        created.status,
        200,
        "CreateAccessPoint {token:?} must succeed, got {} {}",
        created.status,
        created.text()
    );
    let answered = created.json();
    let access_point_id = answered["AccessPointId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateAccessPoint must report the id it minted: {}", created.text()))
        .to_string();
    poll_until(
        &format!("access point {access_point_id} is available"),
        || async {
            let read = efs(
                router,
                "GET",
                &format!("{ACCESS_POINTS_URI}?AccessPointId={access_point_id}"),
                None,
            )
            .await;
            read.json()["AccessPoints"][0]["LifeCycleState"].as_str() == Some("available")
        },
    )
    .await;
    answered["AccessPointArn"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateAccessPoint must report the ARN it minted: {}", created.text()))
        .to_string()
}

/// How many bytes Amazon EFS's own description of `file_system_id` reports as the data it
/// holds (`FileSystemDescription$SizeInBytes`).
async fn metered_bytes(router: &MultiServiceRouter, file_system_id: &str) -> i64 {
    let read = efs(
        router,
        "GET",
        &format!("{FILE_SYSTEMS_URI}?FileSystemId={file_system_id}"),
        None,
    )
    .await;
    read.json()["FileSystems"][0]["SizeInBytes"]["Value"]
        .as_i64()
        .unwrap_or_else(|| panic!("the metered size of {file_system_id}: {}", read.text()))
}

/// Create the probe function `name` as `role_arn`, mounting `access_point` at `mount_path`
/// inside the VPC the subnet and group belong to, and wait until Lambda reports it `Active`.
#[allow(clippy::too_many_arguments)]
async fn create_function(
    router: &MultiServiceRouter,
    name: &str,
    role_arn: &str,
    access_point: &str,
    mount_path: &str,
    subnet_id: &str,
    group_id: &str,
) {
    let created = lambda(
        router,
        "POST",
        FUNCTIONS_URI,
        Some(&json!({
            "FunctionName": name,
            "Role": role_arn,
            "Runtime": RUNTIME,
            "Handler": "index.handler",
            "Timeout": FUNCTION_TIMEOUT_SECONDS,
            "MemorySize": 256,
            "VpcConfig": { "SubnetIds": [subnet_id], "SecurityGroupIds": [group_id] },
            "FileSystemConfigs": [{ "Arn": access_point, "LocalMountPath": mount_path }],
            "Environment": { "Variables": { "MOUNT_PATH": mount_path } },
            "Code": { "ZipFile": base64::engine::general_purpose::STANDARD.encode(package(&[(
                "index.py",
                PROBE_SOURCE,
            )])) },
        })),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "CreateFunction {name:?} must succeed, got {} {}",
        created.status,
        created.text()
    );
    poll_until(&format!("function {name} is active"), || async {
        let read = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
            None,
        )
        .await;
        read.json()["State"].as_str() == Some("Active")
    })
    .await;
}

/// Look again until `holds` answers, or until [`SETTLE_DEADLINE`] has passed — which fails the
/// suite, because the state it waits for is one the clock owes.
async fn poll_until<F, Fut>(what: &str, mut holds: F)
where
    F: FnMut() -> Fut,
    Fut: ::std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        if holds().await {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "expected that {what}, which never happened"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One `Invoke` of `name` with `event`, and what the handler answered — or `None` when the run
/// was ended by a bound this whole test binary shares rather than by anything the case asked
/// for.
async fn answered(
    router: &MultiServiceRouter,
    name: &str,
    case: &str,
    event: &Value,
) -> Option<Value> {
    let uri = format!("{FUNCTIONS_URI}/{name}/invocations");
    let mut answer = lambda(router, "POST", &uri, Some(event)).await;
    for _ in 1..HOST_BOUND_ATTEMPTS {
        if !is_host_bound(answer.status, &answer.text()) {
            break;
        }
        answer = lambda(router, "POST", &uri, Some(event)).await;
    }
    if is_host_bound(answer.status, &answer.text()) {
        return None;
    }
    assert_eq!(
        answer.status,
        200,
        "case {case}: expected 200, got {} {}",
        answer.status,
        answer.text()
    );
    let failure = answer.header("x-amz-function-error");
    assert_eq!(
        failure, None,
        "case {case}: the handler reads and writes its own mount and catches every refusal \
         itself, so the invocation carries no function error, got {failure:?} with {}",
        answer.text()
    );
    Some(answer.json())
}

/// The whole rule, driven END TO END as one test: one file system, two access points of it,
/// four functions mounting them and the invocations that read and write those mounts.
///
/// One test and one stream of invocations, the way the sibling runtime suites are built. A case
/// whose run was stopped by a bound this whole binary shares says nothing about the rule and is
/// left out of the reading — but the test answers how many of its cases a RUN answered, and one
/// that read none at all is a test that asserted nothing, so it fails rather than passing
/// silently.
#[tokio::test]
async fn a_function_reads_and_writes_the_access_point_it_mounts() {
    let _stream = one_invocation_stream_at_a_time().await;
    let (router, _engine) = emulator();
    let ec2 = ec2_client(&EmbeddedEmulator::from_router(router.clone()));
    let net = network(&ec2, "the file system's own VPC", 37, 1).await;
    let subnet = net.subnet_ids.first().expect("one subnet").clone();
    let file_system = available_file_system(&router, "efs-mount-probe", &subnet, &net.group_id).await;

    // The two access points of that ONE file system: a shared subtree with an identity the
    // access point enforces, and another subtree with none, which is what confines each
    // function to its own root and what decides the user its process runs as.
    let shared = available_access_point(
        &router,
        &file_system,
        "ap-shared",
        "/shared",
        Some((ACCESS_POINT_UID, ACCESS_POINT_GID)),
    )
    .await;
    let other = available_access_point(&router, &file_system, "ap-other", "/export/lambda", None)
        .await;
    // And one rooted at the file system's own root, enforcing another identity: a client of it
    // sees every subtree of the file system, so it is what reads back WHO a file another
    // access point's function wrote belongs to.
    let whole = available_access_point(&router, &file_system, "ap-whole", "/", Some((0, 0))).await;

    // The two execution roles: one granted both calls an NFS client of an Amazon EFS file
    // system makes, one granted only the mount. Both carry the network-interface grant a VPC
    // attachment takes, because Lambda stands that attachment up as the function's own role.
    let writing = given_role(
        &router,
        "the role granted ClientMount and ClientWrite",
        "efs-writer",
        Some(json!([
            logs_only(),
            allow(
                "ec2",
                &[
                    "CreateNetworkInterface",
                    "DescribeNetworkInterfaces",
                    "DeleteNetworkInterface",
                ],
                "*",
            ),
            allow("elasticfilesystem", &["ClientMount", "ClientWrite"], "*"),
        ])),
    )
    .await;
    let mounting = given_role(
        &router,
        "the role granted ClientMount only",
        "efs-mounter",
        Some(json!([
            logs_only(),
            allow(
                "ec2",
                &[
                    "CreateNetworkInterface",
                    "DescribeNetworkInterfaces",
                    "DeleteNetworkInterface",
                ],
                "*",
            ),
            allow("elasticfilesystem", &["ClientMount"], "*"),
        ])),
    )
    .await;

    // Four functions over those two access points: the one that writes, another that mounts
    // the SAME access point at a different path, one whose role may not write, and one mounting
    // the other access point of the same file system.
    create_function(&router, "efs-writer-fn", &writing, &shared, "/mnt/data", &subnet, &net.group_id).await;
    create_function(&router, "efs-reader-fn", &writing, &shared, "/mnt/shared", &subnet, &net.group_id).await;
    create_function(&router, "efs-readonly-fn", &mounting, &shared, "/mnt/data", &subnet, &net.group_id).await;
    create_function(&router, "efs-other-fn", &writing, &other, "/mnt/data", &subnet, &net.group_id).await;
    create_function(&router, "efs-whole-fn", &writing, &whole, "/mnt/fs", &subnet, &net.group_id).await;

    let mut read = 0;
    read += the_mount_is_mounted_and_the_identity_is_the_access_points(&router).await;
    read += what_one_function_writes_another_reads(&router, &file_system).await;
    read += a_role_without_the_write_grant_has_it_read_only(&router).await;
    read += each_access_point_confines_the_function_to_its_own_root(&router).await;
    read += an_entry_belongs_to_the_function_that_created_it(&router).await;
    read += what_the_file_system_has_no_room_for_is_refused(&router).await;
    read += the_mount_stands_after_the_configuration_is_updated(&router, &subnet, &net.group_id)
        .await;
    assert!(
        read > 0,
        "expected at least one case to be answered by a run, got {read}: every invocation \
         reported a bound this whole test binary shares, so nothing of the rule was read"
    );
}

/// The mount point is a MOUNT of its own, and the handler's process is the identity the access
/// point enforces — or the user a Lambda execution environment runs as where it enforces none.
///
/// Answers how many cases a run answered.
async fn the_mount_is_mounted_and_the_identity_is_the_access_points(
    router: &MultiServiceRouter,
) -> usize {
    // (case, the function, the path it mounts at, the identity its process reports)
    let rows: [(&str, &str, &str, i64, i64); 3] = [
        (
            "the access point's own PosixUser",
            "efs-writer-fn",
            "/mnt/data",
            ACCESS_POINT_UID,
            ACCESS_POINT_GID,
        ),
        (
            "the same access point at another mount path",
            "efs-reader-fn",
            "/mnt/shared",
            ACCESS_POINT_UID,
            ACCESS_POINT_GID,
        ),
        (
            "an access point that enforces no identity",
            "efs-other-fn",
            "/mnt/data",
            SANDBOX_UID,
            SANDBOX_GID,
        ),
    ];
    let mut read = 0;
    for (case, function, mount_path, uid, gid) in rows {
        let Some(document) = answered(router, function, case, &json!({ "case": "report" })).await
        else {
            continue;
        };
        assert_eq!(
            document,
            json!({
                "mount": mount_path,
                "ismount": true,
                "isdir": true,
                "writable": true,
                "uid": uid,
                "gid": gid,
                "euid": uid,
                "egid": gid,
            }),
            "case {case:?}: expected {mount_path} to be a mounted directory the process of \
             {uid}:{gid} can reach, got {document}"
        );
        read += 1;
    }
    read
}

/// What a handler writes under its mount point is the FILE SYSTEM's: the function that wrote it
/// reads it back, another function that mounts the same file system at another path reads the
/// same bytes, and Amazon EFS's own description of the file system meters them.
///
/// Answers how many cases a run answered.
async fn what_one_function_writes_another_reads(
    router: &MultiServiceRouter,
    file_system: &str,
) -> usize {
    let before = metered_bytes(router, file_system).await;
    // (case, the event the writer is given, what it must answer)
    let rows: Vec<(&str, Value, Value)> = vec![
        (
            "a text write read back",
            json!({ "case": "write", "name": "notes.txt", "text": "first\nsecond\n" }),
            json!({
                "written": true,
                "read_back": "first\nsecond\n",
                "size": 13,
                "lines": ["first\n", "second\n"],
            }),
        ),
        (
            "bytes that are no text at all, written and read back",
            json!({ "case": "write_bytes", "name": "blob.bin", "text": "zero one" }),
            json!({ "written": true, "read_back_is_what_was_written": true, "size": 14 }),
        ),
        (
            "an append onto what is already there",
            json!({ "case": "append", "name": "notes.txt", "text": "third\n" }),
            json!({ "appended": true, "read_back": "first\nsecond\nthird\n" }),
        ),
        (
            "a directory the handler makes, and a file in it",
            json!({ "case": "tree", "name": "deep", "text": "under it" }),
            json!({ "made": true, "is_directory": true, "read_back": "under it" }),
        ),
    ];
    let mut read = 0;
    for (case, event, expected) in rows {
        let Some(document) = answered(router, "efs-writer-fn", case, &event).await else {
            continue;
        };
        assert_eq!(
            document, expected,
            "case {case:?}: expected {expected}, got {document}"
        );
        read += 1;
    }
    if read == 0 {
        return 0;
    }
    // Another function, mounting the same access point at a path of its own: the files are the
    // file system's, so it reads exactly what the writer left.
    let case = "another function reads what the first wrote";
    if let Some(document) = answered(
        router,
        "efs-reader-fn",
        case,
        &json!({ "case": "read", "name": "notes.txt" }),
    )
    .await
    {
        assert_eq!(
            document,
            json!({ "text": "first\nsecond\nthird\n" }),
            "case {case:?}: the second function reads the file system's own file, got {document}"
        );
        read += 1;
    }
    let case = "the directory the first function filled";
    if let Some(document) =
        answered(router, "efs-reader-fn", case, &json!({ "case": "list" })).await
    {
        assert_eq!(
            document,
            json!({ "names": ["blob.bin", "deep", "notes.txt"] }),
            "case {case:?}: the mount lists what was written under it, got {document}"
        );
        read += 1;
    }
    // And Amazon EFS itself meters the bytes: a file system a client has written to reports
    // the size of what it holds, which is what makes the write one that really reached it.
    let metered = metered_bytes(router, file_system).await;
    assert!(
        metered > before,
        "the file system must meter the bytes the handlers wrote, got {metered} after {before}"
    );
    read
}

/// A function whose execution role is short of `elasticfilesystem:ClientWrite` has the file
/// system mounted READ-ONLY: a write under the mount point is refused with `EROFS`, while the
/// reads answer.
///
/// Answers how many cases a run answered.
async fn a_role_without_the_write_grant_has_it_read_only(router: &MultiServiceRouter) -> usize {
    let mut read = 0;
    // (case, the event, what the handler must answer)
    let rows: Vec<(&str, Value, Value)> = vec![
        (
            "a write under a mount the role may not write",
            json!({ "case": "write", "name": "refused.txt", "text": "no" }),
            json!({ "written": false, "errno": "EROFS" }),
        ),
        (
            "a directory under it",
            json!({ "case": "tree", "name": "refused", "text": "no" }),
            json!({ "made": false, "errno": "EROFS" }),
        ),
        (
            "a read of what is already there",
            json!({ "case": "read", "name": "notes.txt" }),
            json!({ "text": "first\nsecond\nthird\n" }),
        ),
    ];
    for (case, event, expected) in rows {
        let Some(document) = answered(router, "efs-readonly-fn", case, &event).await else {
            continue;
        };
        assert_eq!(
            document, expected,
            "case {case:?}: expected {expected}, got {document}"
        );
        read += 1;
    }
    read
}

/// An access point is "an application-specific view into an EFS file system": a function
/// mounting one reaches that access point's own `RootDirectory` and nothing else of the file
/// system, so the other access point's subtree is empty however much the first function wrote.
///
/// Answers how many cases a run answered.
async fn each_access_point_confines_the_function_to_its_own_root(
    router: &MultiServiceRouter,
) -> usize {
    let mut read = 0;
    let case = "the other access point's own root directory";
    if let Some(document) = answered(router, "efs-other-fn", case, &json!({ "case": "list" })).await
    {
        assert_eq!(
            document,
            json!({ "names": [] }),
            "case {case:?}: the other access point shows its own subtree only, got {document}"
        );
        read += 1;
    }
    let case = "a file of the other access point's root is absent from this one";
    if let Some(document) = answered(
        router,
        "efs-other-fn",
        case,
        &json!({ "case": "read", "name": "notes.txt" }),
    )
    .await
    {
        assert_eq!(
            document,
            json!({ "errno": "ENOENT" }),
            "case {case:?}: nothing of the other subtree is reachable here, got {document}"
        );
        read += 1;
    }
    // What this function writes is its own subtree's, and the first function does not see it.
    let case = "a write under the other access point";
    if let Some(document) = answered(
        router,
        "efs-other-fn",
        case,
        &json!({ "case": "write", "name": "mine.txt", "text": "only here" }),
    )
    .await
    {
        assert_eq!(
            document,
            json!({
                "written": true,
                "read_back": "only here",
                "size": 9,
                "lines": ["only here"],
            }),
            "case {case:?}: the write lands in this access point's own root, got {document}"
        );
        read += 1;
    }
    let case = "the first access point does not hold it";
    if let Some(document) = answered(
        router,
        "efs-writer-fn",
        case,
        &json!({ "case": "read", "name": "mine.txt" }),
    )
    .await
    {
        assert_eq!(
            document,
            json!({ "errno": "ENOENT" }),
            "case {case:?}: each access point confines the function that mounts it, got \
             {document}"
        );
        read += 1;
    }
    read
}

/// What the file system has no room for, and a call that can mean nothing, are refused as the
/// filesystem refuses them — a file grown past what one file system holds answers `ENOSPC`, a
/// directory moved inside itself `EINVAL` — and an ordinary descriptor of one of these files
/// duplicates and reads at an offset.
///
/// Answers how many cases a run answered.
async fn what_the_file_system_has_no_room_for_is_refused(router: &MultiServiceRouter) -> usize {
    let mut read = 0;
    // (case, the size the handler asks a file to grow to, what the filesystem answers)
    let rows: [(&str, u64, Value); 3] = [
        ("a size past what one file system holds", 1 << 40, json!("ENOSPC")),
        ("the largest size a call can name", u64::MAX, json!("ENOSPC")),
        ("a size it has room for", 16, json!(16)),
    ];
    for (case, size, grown) in rows {
        let Some(document) = answered(
            router,
            "efs-writer-fn",
            case,
            &json!({ "case": "refused", "name": format!("bounded-{size}"), "size": size }),
        )
        .await
        else {
            continue;
        };
        assert_eq!(
            document,
            json!({ "grown": grown, "moved": "EINVAL", "by_descriptor": "held" }),
            "case {case:?}: expected the file to answer {grown}, the directory move EINVAL and \
             the descriptor to read the file, got {document}"
        );
        read += 1;
    }
    read
}

/// A configuration the function is UPDATED with leaves the mount standing: the update settles,
/// and the next invocation reads the file system through the path the new configuration names.
///
/// Answers how many cases a run answered.
async fn the_mount_stands_after_the_configuration_is_updated(
    router: &MultiServiceRouter,
    subnet_id: &str,
    group_id: &str,
) -> usize {
    let name = "efs-reader-fn";
    let updated = lambda(
        router,
        "PUT",
        &format!("{FUNCTIONS_URI}/{name}/configuration"),
        Some(&json!({
            "MemorySize": 512,
            "VpcConfig": { "SubnetIds": [subnet_id], "SecurityGroupIds": [group_id] },
        })),
    )
    .await;
    assert_eq!(
        updated.status,
        200,
        "UpdateFunctionConfiguration {name:?} must succeed, got {} {}",
        updated.status,
        updated.text()
    );
    poll_until(&format!("the update of {name} settles"), || async {
        let read = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
            None,
        )
        .await;
        let configuration = read.json();
        configuration["LastUpdateStatus"].as_str() == Some("Successful")
            && configuration["State"].as_str() == Some("Active")
    })
    .await;
    let case = "the mount after an update of the configuration";
    let Some(document) = answered(
        router,
        name,
        case,
        &json!({ "case": "read", "name": "notes.txt" }),
    )
    .await
    else {
        return 0;
    };
    assert_eq!(
        document,
        json!({ "text": "first\nsecond\nthird\n" }),
        "case {case:?}: the updated function still reads the file system, got {document}"
    );
    1
}

/// An entry BELONGS TO the identity that created it: a file one function wrote through an
/// access point that enforces a `PosixUser` reports that user to every other client of the
/// file system, whichever identity THAT client acts as.
///
/// Answers how many cases a run answered.
async fn an_entry_belongs_to_the_function_that_created_it(router: &MultiServiceRouter) -> usize {
    let mut read = 0;
    // (case, the path under the file system's own root, who it must belong to)
    let rows: [(&str, &str, i64, i64); 2] = [
        (
            "a file written through the access point that enforces an identity",
            "shared/notes.txt",
            ACCESS_POINT_UID,
            ACCESS_POINT_GID,
        ),
        (
            "a file written through one that enforces none",
            "export/lambda/mine.txt",
            SANDBOX_UID,
            SANDBOX_GID,
        ),
    ];
    for (case, name, uid, gid) in rows {
        let Some(document) = answered(
            router,
            "efs-whole-fn",
            case,
            &json!({ "case": "stat", "name": name }),
        )
        .await
        else {
            continue;
        };
        assert_eq!(
            (document["exists"].clone(), document["uid"].clone(), document["gid"].clone()),
            (json!(true), json!(uid), json!(gid)),
            "case {case:?}: expected {name} to be there and belong to {uid}:{gid}, got \
             {document}"
        );
        read += 1;
    }
    read
}

/// The probe every row drives: its handler reads and writes the file system mounted at the path
/// its own configuration named, and reports what the filesystem answered — catching every
/// refusal itself and naming the errno, so a case reads the rule rather than an unhandled
/// error.
const PROBE_SOURCE: &str = r#"
import errno
import os

MOUNT = os.environ["MOUNT_PATH"]

# The errno values an NFS client of an Amazon EFS file system answers these calls with, named
# so a case reads the condition rather than a number.
_NAMED = {
    errno.EROFS: "EROFS",
    errno.EACCES: "EACCES",
    errno.ENOENT: "ENOENT",
    errno.EISDIR: "EISDIR",
    errno.ENOSPC: "ENOSPC",
    errno.EEXIST: "EEXIST",
    errno.EINVAL: "EINVAL",
    errno.EXDEV: "EXDEV",
}


def _named(number):
    return _NAMED.get(number, str(number))


def _report():
    return {
        "mount": MOUNT,
        "ismount": os.path.ismount(MOUNT),
        "isdir": os.path.isdir(MOUNT),
        "writable": os.access(MOUNT, os.R_OK),
        "uid": os.getuid(),
        "gid": os.getgid(),
        "euid": os.geteuid(),
        "egid": os.getegid(),
    }


def _wrote(name, text):
    path = os.path.join(MOUNT, name)
    try:
        with open(path, "w") as stream:
            stream.write(text)
    except OSError as error:
        return {"written": False, "errno": _named(error.errno)}
    with open(path) as stream:
        read_back = stream.read()
    with open(path) as stream:
        lines = stream.readlines()
    return {
        "written": True,
        "read_back": read_back,
        "size": os.path.getsize(path),
        "lines": lines,
    }


def _wrote_bytes(name, text):
    path = os.path.join(MOUNT, name)
    # Bytes that are no text at all, so a case reads the FILE rather than its text: a
    # filesystem holds the bytes a program wrote.
    written = text.encode("utf-8") + bytes([0, 1, 2, 0x7F, 0xFF, 0xFE])
    try:
        with open(path, "wb") as stream:
            stream.write(written)
    except OSError as error:
        return {"written": False, "errno": _named(error.errno)}
    with open(path, "rb") as stream:
        read_back = stream.read()
    return {
        "written": True,
        "read_back_is_what_was_written": read_back == written,
        "size": os.stat(path).st_size,
    }


def _appended(name, text):
    path = os.path.join(MOUNT, name)
    try:
        with open(path, "a") as stream:
            stream.write(text)
    except OSError as error:
        return {"appended": False, "errno": _named(error.errno)}
    with open(path) as stream:
        return {"appended": True, "read_back": stream.read()}


def _tree(name, text):
    directory = os.path.join(MOUNT, name)
    try:
        os.mkdir(directory)
    except OSError as error:
        return {"made": False, "errno": _named(error.errno)}
    path = os.path.join(directory, "held.txt")
    with open(path, "w") as stream:
        stream.write(text)
    with open(path) as stream:
        read_back = stream.read()
    return {
        "made": True,
        "is_directory": os.path.isdir(directory),
        "read_back": read_back,
    }


def _read(name):
    path = os.path.join(MOUNT, name)
    try:
        with open(path) as stream:
            return {"text": stream.read()}
    except OSError as error:
        return {"errno": _named(error.errno)}


def _stat(name):
    path = os.path.join(MOUNT, name)
    try:
        held = os.stat(path)
    except OSError as error:
        return {"exists": False, "errno": _named(error.errno)}
    with open(path) as stream:
        body = stream.read()
    return {"exists": True, "body": body, "uid": held.st_uid, "gid": held.st_gid}


def _refused(name, size):
    """What the filesystem answers for the calls it must refuse: a file grown past what the
    file system holds, and a directory moved inside itself."""
    path = os.path.join(MOUNT, name)
    with open(path, "w") as stream:
        stream.write("held")
    answered = {}
    try:
        os.truncate(path, size)
        answered["grown"] = os.path.getsize(path)
    except OSError as error:
        answered["grown"] = _named(error.errno)
    directory = os.path.join(MOUNT, name + "-dir")
    os.mkdir(directory)
    try:
        os.rename(directory, os.path.join(directory, "inside"))
        answered["moved"] = True
    except OSError as error:
        answered["moved"] = _named(error.errno)
    # A descriptor of one of these files is an ordinary descriptor: it duplicates, reads at an
    # offset and closes.
    with open(path, "rb") as stream:
        second = os.dup(stream.fileno())
        answered["by_descriptor"] = os.pread(second, 4, 0).decode("utf-8")
        os.close(second)
    return answered


def handler(event, context):
    case = event["case"]
    if case == "report":
        return _report()
    if case == "write":
        return _wrote(event["name"], event["text"])
    if case == "write_bytes":
        return _wrote_bytes(event["name"], event["text"])
    if case == "append":
        return _appended(event["name"], event["text"])
    if case == "tree":
        return _tree(event["name"], event["text"])
    if case == "read":
        return _read(event["name"])
    if case == "stat":
        return _stat(event["name"])
    if case == "refused":
        return _refused(event["name"], event["size"])
    if case == "list":
        return {"names": sorted(os.listdir(MOUNT))}
    return {"case": case}
"#;
