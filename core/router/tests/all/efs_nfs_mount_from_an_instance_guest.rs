// SPDX-License-Identifier: CC0-1.0

//! Mounting an Amazon EFS file system FROM INSIDE an instance's guest, on the whole assembled
//! emulator a client talks to.
//!
//! RULE under test: once the control plane reports a mount target `available`, the file system
//! is SERVED at the address that mount target reports — a client in the VPC mounts it with
//! `mount -t nfs4 <IpAddress>:/ <dir>`, a file written through the mount is read back
//! byte for byte, the mount is genuinely a network filesystem (`findmnt -no FSTYPE <dir>` is
//! the mount's own type and `stat -f -c %T <dir>` the type a statfs of it reports), and every
//! client that mounts the SAME file system reaches the same files, so what one instance writes
//! another instance in another Availability Zone reads. An address no `available` mount target
//! of that VPC answers at serves nothing, and the mount fails the way `mount` fails.
//!
//! This suite drives the ASSEMBLED router — every service mounted on one engine, as
//! `core/router/src/lib.rs` assembles it — because the behaviour is derived across four of
//! those services: a client creates the file system and its mount targets through Amazon EFS,
//! the subnets they are placed in and the instances that mount them through Amazon EC2, the
//! role those instances run as through IAM, and then runs the mount script through Systems
//! Manager's Run Command, which is what delivers a program to a guest. Nothing reaches into a
//! state struct: every fact is written and read through the service that owns it, and the mount
//! itself happens inside the guest, as the script's own `mount` command. Assembling the whole
//! router costs stack (see `boot_serve.rs`), so it is built — and the requests driven — on a
//! sized thread.
//!
//! Nothing here waits a fixed time: every span this suite crosses — Amazon EC2's move to
//! `running`, the node's registration, the file system's and the mount targets' own
//! `creating` spans, and the invocation's move off `Pending` — is a stored due instant, so a
//! case LOOKS AGAIN until the condition holds or [`DEADLINE`] passes, and no assertion is made
//! about elapsed time.
//!
//! The rows of the mount table span what the rule generalises over: the address mounted (a
//! mount target's own, and one of the subnet nothing answers at), which node mounts it (two
//! instances in two Availability Zones, each through its own mount target), and which
//! direction a file crosses between them.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_of, element, elements, form_encode};

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The EC2 API version a client sends in an ec2Query body.
const EC2_API_VERSION: &str = "2016-11-15";

/// The IAM API version a client sends in an awsQuery body.
const IAM_API_VERSION: &str = "2010-05-08";

/// The public Systems Manager parameter a client resolves the Amazon Linux 2023 image id
/// through, which is the image these instances are launched from.
const AL2023_PARAMETER: &str =
    "/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64";

/// The AWS-managed policy a client attaches to an instance profile's role so the agent in the
/// guest registers the node and takes the commands sent to it.
const SSM_CORE_POLICY: &str = "arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore";

/// The service principal a role has to admit for Amazon EC2 to act as it on an instance's
/// behalf.
const EC2_PRINCIPAL: &str = "ec2.amazonaws.com";

/// The document Run Command runs a shell script on a Linux node with.
const SHELL_DOCUMENT: &str = "AWS-RunShellScript";

/// The instance state a client's `instance_running` waiter waits for.
const RUNNING: &str = "running";

/// The `PingStatus` of a node whose agent is responding.
const ONLINE: &str = "Online";

/// The invocation statuses a client polls `GetCommandInvocation` for.
const SUCCESS_STATUS: &str = "Success";
const FAILED_STATUS: &str = "Failed";

/// The statuses an invocation has reached its end in
/// (`smithy_query(query="shape",shape="CommandInvocationStatus")`).
const TERMINAL_STATUSES: &[&str] = &["Success", "Cancelled", "TimedOut", "Failed"];

/// The directory the scripts below mount the file system at.
const MOUNT_POINT: &str = "/mnt/sim2real-efs";

/// The options the documented Amazon EFS mount command carries.
const MOUNT_OPTIONS: &str =
    "nfsvers=4.1,rsize=1048576,wsize=1048576,hard,timeo=600,retrans=2,noresvport";

/// The filesystem type a listing of the mounts reports for an Amazon EFS mount, and the type a
/// statfs of a path on it reports — the two independent probes a client confirms a mount with.
const MOUNT_FSTYPE: &str = "nfs4";
const STATFS_TYPE: &str = "nfs";

/// The exit code `mount` fails with: "32 mount failure" (`mount(8)`, EXIT STATUS).
const MOUNT_FAILURE: i64 = 32;

/// How long a case is given for a condition another component's clock owes.
///
/// Generous on purpose: each span is a stored due instant and nothing sleeps for one, so a
/// case looks again until the condition holds. All this bound is for is that a move that never
/// comes fails instead of hanging; nothing asserts on elapsed time.
const DEADLINE: Duration = Duration::from_secs(120);

/// How long one look-again cycle pauses before polling again. Not a bound on anything.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request and where its Region comes from; the signature is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=deadbeef"
    )
}

/// One answer, reduced to what a client branches on.
#[derive(Debug)]
struct Answer {
    status: u16,
    /// The body as the service wrote it, which is what an XML wire is read from.
    text: String,
    /// The same body parsed, for a JSON wire.
    body: Value,
}

/// One request this suite sends: where it goes, and what it carries.
struct Call<'a> {
    /// The signing name whose endpoint it is sent to, which is what routes it.
    signing_name: &'a str,
    method: &'a str,
    path: &'a str,
    content_type: &'a str,
    /// `x-amz-target`, for the RPC wires that carry the operation in a header.
    target: Option<String>,
    body: String,
}

/// One request at `call.signing_name`'s endpoint in [`REGION`].
fn send(router: &MultiServiceRouter, runtime: &tokio::runtime::Runtime, call: Call<'_>) -> Answer {
    let Call {
        signing_name,
        method,
        path,
        content_type,
        target,
        body,
    } = call;
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!(
            "https://{signing_name}.{REGION}.amazonaws.com{path}"
        ))
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header("content-type", content_type);
    if let Some(target) = target {
        builder = builder.header("x-amz-target", target);
    }
    let request = builder
        .body(bytes::Bytes::from(body))
        .expect("a well-formed request");
    let response = runtime.block_on(router.dispatch(request));
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    let body = serde_json::from_str(&text).unwrap_or(Value::Null);
    Answer { status, text, body }
}

/// A form body carrying `action`, `version` and the parameters as written.
fn form_body(action: &str, version: &str, parameters: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in parameters {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    body
}

/// One form-encoded action at `signing_name`'s endpoint — the ec2Query and awsQuery wires.
fn form_call<'a>(signing_name: &'a str, version: &str, action: &str, parameters: &[(&str, &str)]) -> Call<'a> {
    Call {
        signing_name,
        method: "POST",
        path: "/",
        content_type: "application/x-www-form-urlencoded",
        target: None,
        body: form_body(action, version, parameters),
    }
}

/// One ec2Query action of Amazon EC2 that must be served, as its XML body.
fn ec2_ok(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    action: &str,
    parameters: &[(&str, &str)],
) -> String {
    let answer = send(
        router,
        runtime,
        form_call("ec2", EC2_API_VERSION, action, parameters),
    );
    assert_eq!(
        answer.status, 200,
        "case {case:?}: Amazon EC2 {action} must be served, got {answer:?}"
    );
    answer.text
}

/// One awsQuery action of IAM that must be served.
fn iam_ok(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    action: &str,
    parameters: &[(&str, &str)],
) {
    let answer = send(
        router,
        runtime,
        form_call("iam", IAM_API_VERSION, action, parameters),
    );
    assert_eq!(
        answer.status, 200,
        "case {case:?}: IAM {action} must be served, got {answer:?}"
    );
}

/// One Systems Manager operation over its awsJson1_1 wire that must be served.
fn ssm_ok(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let answer = send(
        router,
        runtime,
        Call {
            signing_name: "ssm",
            method: "POST",
            path: "/",
            content_type: "application/x-amz-json-1.1",
            target: Some(format!("AmazonSSM.{operation}")),
            body: body.to_string(),
        },
    );
    assert_eq!(
        answer.status, 200,
        "case {case:?}: Systems Manager {operation} must be served, got {answer:?}"
    );
    answer.body
}

/// One Amazon EFS route over its restJson1 wire.
fn efs(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answer {
    send(
        router,
        runtime,
        Call {
            signing_name: "elasticfilesystem",
            method,
            path,
            content_type: "application/json",
            target: None,
            body: body.map(|value| value.to_string()).unwrap_or_default(),
        },
    )
}

/// One Amazon EFS route that must be served, as its parsed body.
fn efs_ok(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Value {
    let answer = efs(router, runtime, method, path, body);
    assert!(
        (200..300).contains(&answer.status),
        "case {case:?}: Amazon EFS {method} {path} must be served, got {answer:?}"
    );
    answer.body
}

/// A trust policy admitting `principal` and nothing else, written with none of `&`, `=`, `+`
/// or `%` in it.
fn trust_policy(principal: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"Service\":\"{principal}\"}},\"Action\":\"sts:AssumeRole\"}}]}}"
    )
}

/// Build the instance profile the instances below run as, the way a client builds one, so the
/// agent in each guest registers its node and takes the commands sent to it.
fn create_profile(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    profile: &str,
    role: &str,
) {
    let trust = trust_policy(EC2_PRINCIPAL);
    iam_ok(
        router,
        runtime,
        profile,
        "CreateRole",
        &[("RoleName", role), ("AssumeRolePolicyDocument", &trust)],
    );
    iam_ok(
        router,
        runtime,
        profile,
        "AttachRolePolicy",
        &[("RoleName", role), ("PolicyArn", SSM_CORE_POLICY)],
    );
    iam_ok(
        router,
        runtime,
        profile,
        "CreateInstanceProfile",
        &[("InstanceProfileName", profile)],
    );
    iam_ok(
        router,
        runtime,
        profile,
        "AddRoleToInstanceProfile",
        &[("InstanceProfileName", profile), ("RoleName", role)],
    );
}

/// The image id the public Systems Manager parameter resolves to.
fn image_id(router: &MultiServiceRouter, runtime: &tokio::runtime::Runtime) -> String {
    let answered = ssm_ok(
        router,
        runtime,
        "the image",
        "GetParameter",
        &json!({ "Name": AL2023_PARAMETER }),
    );
    answered["Parameter"]["Value"]
        .as_str()
        .unwrap_or_else(|| panic!("GetParameter must report an image id, got {answered}"))
        .to_string()
}

/// Create a VPC of this suite's own and one subnet per Availability Zone in it, as the
/// repro of a client that provisions a dedicated network for its file system does — and
/// answer the two subnet ids.
///
/// A network of its own rather than the account's default one, so the addresses the mount
/// targets below report come out of a block this suite named.
fn dedicated_subnets(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
) -> [String; 2] {
    let created = ec2_ok(
        router,
        runtime,
        "the network",
        "CreateVpc",
        &[("CidrBlock", "10.91.0.0/16")],
    );
    let vpc = element(&created, "vpcId");
    let mut subnets: Vec<String> = Vec::new();
    for (index, zone) in [format!("{REGION}a"), format!("{REGION}b")]
        .iter()
        .enumerate()
    {
        let block = format!("10.91.{index}.0/24");
        let created = ec2_ok(
            router,
            runtime,
            zone,
            "CreateSubnet",
            &[
                ("VpcId", &vpc),
                ("CidrBlock", &block),
                ("AvailabilityZone", zone),
            ],
        );
        subnets.push(element(&created, "subnetId"));
    }
    [subnets[0].clone(), subnets[1].clone()]
}

/// Launch one instance in `subnet`, running as `profile`, as the instance id Amazon EC2
/// answered with.
fn launch(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    image: &str,
    subnet: &str,
    profile: &str,
) -> String {
    let launched = ec2_ok(
        router,
        runtime,
        case,
        "RunInstances",
        &[
            ("ImageId", image),
            ("MinCount", "1"),
            ("MaxCount", "1"),
            ("SubnetId", subnet),
            ("IamInstanceProfile.Name", profile),
        ],
    );
    element(&launched, "instanceId")
}

/// Look again until Amazon EC2 reports `instance_id` running — the condition a client's
/// `instance_running` waiter waits for — and then until Systems Manager reports it as a
/// managed node whose agent is responding.
fn when_managed(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    instance_id: &str,
) {
    let until = Instant::now() + DEADLINE;
    loop {
        let described = ec2_ok(
            router,
            runtime,
            case,
            "DescribeInstances",
            &[("InstanceId.1", instance_id)],
        );
        let state = block_of(&described, "instanceState").unwrap_or_default();
        if elements(state, "name").iter().any(|name| name == RUNNING) {
            break;
        }
        assert!(
            Instant::now() < until,
            "case {case:?}: expected Amazon EC2 to report {instance_id} {RUNNING}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
    loop {
        let reported = ssm_ok(
            router,
            runtime,
            case,
            "DescribeInstanceInformation",
            &json!({ "Filters": [{ "Key": "InstanceIds", "Values": [instance_id] }] }),
        );
        let online = reported["InstanceInformationList"]
            .as_array()
            .is_some_and(|listed| {
                listed
                    .iter()
                    .any(|node| node["PingStatus"].as_str() == Some(ONLINE))
            });
        if online {
            break;
        }
        assert!(
            Instant::now() < until,
            "case {case:?}: expected {instance_id} to become a managed node reporting {ONLINE}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Create one file system and look again until it reports `available`, which is the state a
/// mount target can be created in.
fn available_file_system(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    token: &str,
) -> String {
    let created = efs_ok(
        router,
        runtime,
        token,
        "POST",
        "/2015-02-01/file-systems",
        Some(json!({ "CreationToken": token })),
    );
    let file_system_id = created["FileSystemId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateFileSystem must mint an id, got {created}"))
        .to_string();
    let until = Instant::now() + DEADLINE;
    loop {
        let described = efs_ok(
            router,
            runtime,
            token,
            "GET",
            &format!("/2015-02-01/file-systems?FileSystemId={file_system_id}"),
            None,
        );
        if described["FileSystems"][0]["LifeCycleState"].as_str() == Some("available") {
            return file_system_id;
        }
        assert!(
            Instant::now() < until,
            "case {token:?}: expected {file_system_id} to reach available, last read \
             {described}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Create one mount target for `file_system_id` in `subnet` and look again until the control
/// plane reports it `available`, answering the address it reports — which is what a client
/// mounts the file system by.
fn available_mount_target(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    file_system_id: &str,
    subnet: &str,
) -> String {
    let created = efs_ok(
        router,
        runtime,
        case,
        "POST",
        "/2015-02-01/mount-targets",
        Some(json!({ "FileSystemId": file_system_id, "SubnetId": subnet })),
    );
    let mount_target_id = created["MountTargetId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateMountTarget must mint an id, got {created}"))
        .to_string();
    let until = Instant::now() + DEADLINE;
    loop {
        let described = efs_ok(
            router,
            runtime,
            case,
            "GET",
            &format!("/2015-02-01/mount-targets?MountTargetId={mount_target_id}"),
            None,
        );
        let reported = &described["MountTargets"][0];
        if reported["LifeCycleState"].as_str() == Some("available") {
            return reported["IpAddress"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: an available mount target must report an address, got {described}")
                })
                .to_string();
        }
        assert!(
            Instant::now() < until,
            "case {case:?}: expected {mount_target_id} to reach available, last read \
             {described}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Send `commands` to the guest of `instance_id` as one shell script, and look again until the
/// invocation has reached a terminal status — answering the whole `GetCommandInvocation`
/// report, which is what a client reads the script's two streams and its exit code out of.
fn ran_in_guest(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    instance_id: &str,
    commands: &[String],
) -> Value {
    let sent = ssm_ok(
        router,
        runtime,
        case,
        "SendCommand",
        &json!({
            "DocumentName": SHELL_DOCUMENT,
            "InstanceIds": [instance_id],
            "Parameters": { "commands": commands },
        }),
    );
    let command_id = sent["Command"]["CommandId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: SendCommand must mint a command id, got {sent}"))
        .to_string();
    let until = Instant::now() + DEADLINE;
    loop {
        let reported = ssm_ok(
            router,
            runtime,
            case,
            "GetCommandInvocation",
            &json!({ "CommandId": command_id, "InstanceId": instance_id }),
        );
        if reported["Status"]
            .as_str()
            .is_some_and(|status| TERMINAL_STATUSES.contains(&status))
        {
            return reported;
        }
        assert!(
            Instant::now() < until,
            "case {case:?}: expected the invocation of {command_id} to reach a terminal status, \
             last read {reported}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// The script a client mounts the file system at `address` with, writes `marker` through, reads
/// it back, and probes the two independent filesystem facts with — the documented way of
/// confirming an Amazon EFS mount.
fn mount_write_read_script(address: &str, marker: &str) -> Vec<String> {
    vec![
        "set -e".to_string(),
        format!("sudo mkdir -p {MOUNT_POINT}"),
        format!("sudo mount -t nfs4 -o {MOUNT_OPTIONS} {address}:/ {MOUNT_POINT}"),
        format!("echo {marker} | sudo tee {MOUNT_POINT}/{marker}.txt"),
        format!("echo CONTENT:$(cat {MOUNT_POINT}/{marker}.txt)"),
        format!("echo FSTYPE:$(findmnt -no FSTYPE {MOUNT_POINT})"),
        format!("echo MAGIC:$(stat -f -c %T {MOUNT_POINT})"),
        format!("sudo umount {MOUNT_POINT}"),
    ]
}

/// What the script wrote on standard output, as `GetCommandInvocation` reports it.
fn written(report: &Value) -> String {
    report["StandardOutputContent"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[test]
fn an_available_mount_target_is_mounted_written_and_read_from_inside_the_guest() {
    std::thread::Builder::new()
        .name("efs-nfs-mount-in-a-guest".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            // Signatures are not what this suite is about; the router still reads the SigV4
            // credential scope, which is where each call's Region comes from.
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("a current-thread runtime");

            create_profile(&router, &runtime, "efs-nfs-node", "efs-nfs-node-role");
            let image = image_id(&router, &runtime);
            let [first_subnet, second_subnet] = dedicated_subnets(&router, &runtime);

            // The file system and one mount target per zone, each waited to `available` —
            // which is the signal a client mounts on.
            let file_system = available_file_system(&router, &runtime, "efs-nfs-round-trip");
            let first_address = available_mount_target(
                &router,
                &runtime,
                "the first zone",
                &file_system,
                &first_subnet,
            );
            let second_address = available_mount_target(
                &router,
                &runtime,
                "the second zone",
                &file_system,
                &second_subnet,
            );
            assert_ne!(
                first_address, second_address,
                "each mount target reports an address of its own subnet"
            );

            // One instance per zone, each a managed node its agent takes commands for.
            let node_a = launch(
                &router,
                &runtime,
                "node A",
                &image,
                &first_subnet,
                "efs-nfs-node",
            );
            let node_b = launch(
                &router,
                &runtime,
                "node B",
                &image,
                &second_subnet,
                "efs-nfs-node",
            );
            when_managed(&router, &runtime, "node A", &node_a);
            when_managed(&router, &runtime, "node B", &node_b);

            // Node A: mount, write a marker through the mount, read it back, and probe the two
            // independent filesystem facts.
            let marker = "marker-from-node-a";
            let report = ran_in_guest(
                &router,
                &runtime,
                "node A's round trip",
                &node_a,
                &mount_write_read_script(&first_address, marker),
            );
            assert_eq!(
                report["Status"].as_str(),
                Some(SUCCESS_STATUS),
                "node A's mount/write/read script must succeed, got {report}"
            );
            let output = written(&report);
            for expected in [
                format!("CONTENT:{marker}"),
                format!("FSTYPE:{MOUNT_FSTYPE}"),
                format!("MAGIC:{STATFS_TYPE}"),
            ] {
                assert!(
                    output.lines().any(|line| line == expected),
                    "node A's script must write {expected:?}, got {output:?}"
                );
            }

            // Node B, in the other zone and through the other mount target: the file node A
            // wrote is a file of the FILE SYSTEM, so this node reads it back byte for byte.
            let answer = "marker-from-node-b";
            let coherence = vec![
                "set -e".to_string(),
                format!("sudo mkdir -p {MOUNT_POINT}"),
                format!(
                    "sudo mount -t nfs4 -o {MOUNT_OPTIONS} {second_address}:/ {MOUNT_POINT}"
                ),
                format!("echo SEEN:$(cat {MOUNT_POINT}/{marker}.txt)"),
                format!("echo {answer} | sudo tee {MOUNT_POINT}/{answer}.txt"),
                format!("echo FSTYPE:$(findmnt -no FSTYPE {MOUNT_POINT})"),
                format!("sudo umount {MOUNT_POINT}"),
            ];
            let report = ran_in_guest(&router, &runtime, "node B's read", &node_b, &coherence);
            assert_eq!(
                report["Status"].as_str(),
                Some(SUCCESS_STATUS),
                "node B's script must succeed, got {report}"
            );
            let output = written(&report);
            assert!(
                output.lines().any(|line| line == format!("SEEN:{marker}")),
                "node B must read what node A wrote through the shared file system, got \
                 {output:?}"
            );
            assert!(
                output
                    .lines()
                    .any(|line| line == format!("FSTYPE:{MOUNT_FSTYPE}")),
                "node B's mount must be the same kind of filesystem, got {output:?}"
            );

            // ... and back the other way: node A reads what node B wrote, through a fresh
            // mount of the same file system.
            let back = vec![
                "set -e".to_string(),
                format!("sudo mkdir -p {MOUNT_POINT}"),
                format!("sudo mount -t nfs4 -o {MOUNT_OPTIONS} {first_address}:/ {MOUNT_POINT}"),
                format!("echo BACK:$(cat {MOUNT_POINT}/{answer}.txt)"),
                format!("sudo umount {MOUNT_POINT}"),
            ];
            let report = ran_in_guest(&router, &runtime, "node A's read back", &node_a, &back);
            assert_eq!(
                report["Status"].as_str(),
                Some(SUCCESS_STATUS),
                "node A's second script must succeed, got {report}"
            );
            let output = written(&report);
            assert!(
                output.lines().any(|line| line == format!("BACK:{answer}")),
                "node A must read what node B wrote, got {output:?}"
            );

            // An address no mount target of this VPC answers at serves nothing: the script's
            // own mount fails, so the invocation reports the exit code `mount` failed with and
            // none of the probes is ever written.
            let unserved = vec![
                "set -e".to_string(),
                format!("sudo mkdir -p {MOUNT_POINT}"),
                format!("sudo mount -t nfs4 -o {MOUNT_OPTIONS} 10.253.253.253:/ {MOUNT_POINT}"),
                format!("echo FSTYPE:$(findmnt -no FSTYPE {MOUNT_POINT})"),
            ];
            let report = ran_in_guest(
                &router,
                &runtime,
                "an address nothing answers at",
                &node_a,
                &unserved,
            );
            assert_eq!(
                (
                    report["Status"].as_str(),
                    report["ResponseCode"].as_i64(),
                ),
                (Some(FAILED_STATUS), Some(MOUNT_FAILURE)),
                "a mount of an address nothing answers at must fail the way mount fails, got \
                 {report}"
            );
            let output = written(&report);
            assert!(
                !output.contains("FSTYPE:"),
                "nothing must be probed after a mount that did not happen, got {output:?}"
            );
        })
        .expect("a sized thread")
        .join()
        .expect("the suite thread");
}
