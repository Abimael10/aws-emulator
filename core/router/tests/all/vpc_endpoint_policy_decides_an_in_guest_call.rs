// SPDX-License-Identifier: CC0-1.0

//! A VPC ENDPOINT POLICY deciding an AWS call made from INSIDE an instance's guest, on the
//! whole assembled emulator a client talks to.
//!
//! RULE under test: a call an AWS SDK makes inside an instance's guest is a real call made by
//! the role the instance profile carries — so its effects are the account's own — and when the
//! instance's VPC holds a VPC endpoint for the called service, that endpoint's policy decides
//! it: a call the endpoint policy does not allow is refused `AccessDenied` however the role's
//! own IAM policies read, and nothing is written for it; a call it allows travels, and the
//! object the write made is the object a later read of the bucket answers with.
//!
//! This suite drives the ASSEMBLED router — every service mounted on one engine, as
//! `core/router/src/lib.rs` assembles it — because the behaviour is derived across five of
//! those services: a client creates the network, the endpoint and the instance through Amazon
//! EC2, the role the instance runs as through IAM, the bucket through Amazon S3, and then runs
//! the in-guest program through Systems Manager's Run Command, which is what delivers a program
//! to a guest. Nothing reaches into a state struct: every fact is written and read through the
//! service that owns it, and the write itself happens inside the guest, as the program's own
//! `boto3` call. Assembling the whole router costs stack (see `boot_serve.rs`), so it is built
//! — and the requests driven — on a sized thread.
//!
//! Nothing here waits a fixed time: every span this suite crosses — Amazon EC2's move to
//! `running`, the endpoint's move to `available`, the node's registration and the invocation's
//! move off `Pending` — is a stored due instant, so a case LOOKS AGAIN until the condition
//! holds or [`DEADLINE`] passes, and no assertion is made about elapsed time.
//!
//! The rows of the table span what the rule generalises over: WHAT the endpoint's document says
//! (the full-access default, an explicit deny of the write, a document that allows reads alone,
//! a deny scoped to another bucket's objects, and a document naming this one object), and each
//! row drives TWO actions through the endpoint — a write and a read — so a decision scoped to
//! one action is shown to leave the other alone.

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
/// through, which is the image this instance is launched from.
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

/// The state a VPC endpoint serves traffic in.
const ENDPOINT_AVAILABLE: &str = "available";

/// The invocation status a script that ran to success reports.
const SUCCESS_STATUS: &str = "Success";

/// The statuses an invocation has reached its end in
/// (`smithy_query(query="shape",shape="CommandInvocationStatus")`).
const TERMINAL_STATUSES: &[&str] = &["Success", "Cancelled", "TimedOut", "Failed"];

/// The endpoint service of Amazon S3 in this Region, as `CreateVpcEndpoint` names one:
/// `com.amazonaws.<region>.<service>`.
const S3_ENDPOINT_SERVICE: &str = "com.amazonaws.us-east-1.s3";

/// The bucket the in-guest program writes to and reads from.
const BUCKET: &str = "vpce-policy-in-guest-bucket";

/// A second bucket, named by a row's policy and written by nothing: what a deny scoped to
/// ANOTHER bucket's objects is scoped to.
const OTHER_BUCKET: &str = "vpce-policy-other-bucket";

/// The object the in-guest program READS, stored through Amazon S3's own API first so a row
/// reads an object that is there.
const SEEDED_KEY: &str = "seeded/object.txt";

/// What that object holds, which is what a read through the endpoint has to answer with.
const SEEDED_CONTENT: &str = "stored before the program ran";

/// The error code Amazon S3 refuses an unauthorized request with.
const ACCESS_DENIED: &str = "AccessDenied";

/// What the in-guest write carries, which is what a later read of the object answers with.
const WRITTEN_IN_THE_GUEST: &str = "written in the guest";

/// The file of the machine's own storage the CLI script writes its payload into, and the one it
/// reads the seeded object back into: a local path the command names is a path of that machine.
const PAYLOAD_PATH: &str = "/tmp/vpce-policy-payload";
const READ_PATH: &str = "/tmp/vpce-policy-read";

/// The status the AWS CLI exits with when the service refused its request: "254 -- … the service
/// returned an error" (AWS CLI return codes).
const CLI_SERVICE_ERROR: i64 = 254;

/// ... and when it carried the command out: "0 -- Command was successful" (same reference).
const CLI_SUCCEEDED: i64 = 0;

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
        .uri(format!("https://{signing_name}.{REGION}.amazonaws.com{path}"))
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
fn form_call<'a>(
    signing_name: &'a str,
    version: &str,
    action: &str,
    parameters: &[(&str, &str)],
) -> Call<'a> {
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

/// One Amazon S3 call on the path-style address.
fn s3(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    method: &str,
    path: &str,
    body: &str,
) -> Answer {
    send(
        router,
        runtime,
        Call {
            signing_name: "s3",
            method,
            path,
            content_type: "application/octet-stream",
            target: None,
            body: body.to_string(),
        },
    )
}

/// Create `named` through Amazon S3's own API.
fn create_bucket(router: &MultiServiceRouter, runtime: &tokio::runtime::Runtime, named: &str) {
    let created = s3(router, runtime, "PUT", &format!("/{named}"), "");
    assert_eq!(
        created.status, 200,
        "setup: CreateBucket({named:?}) must be served, got {created:?}"
    );
}

/// A trust policy admitting `principal` and nothing else, written with none of `&`, `=`, `+`
/// or `%` in it.
fn trust_policy(principal: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"Service\":\"{principal}\"}},\"Action\":\"sts:AssumeRole\"}}]}}"
    )
}

/// Build the instance profile the instance below runs as, the way a client builds one: the agent
/// in the guest registers its node and takes the commands sent to it, and the role's own
/// identity-based policy ALLOWS the object calls the in-guest program makes — so what a row
/// observes is the endpoint policy's decision and never the role's. It allows the one Amazon EC2
/// read a program of the guest makes too, which is the call that re-enters the service
/// performing the script.
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
    let allows_objects = "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Allow\",\
         \"Action\":[\"s3:PutObject\",\"s3:GetObject\"],\"Resource\":\"arn:aws:s3:::*\"},\
         {\"Effect\":\"Allow\",\"Action\":\"ec2:DescribeSecurityGroups\",\"Resource\":\"*\"}]}";
    iam_ok(
        router,
        runtime,
        profile,
        "PutRolePolicy",
        &[
            ("RoleName", role),
            ("PolicyName", "objects"),
            ("PolicyDocument", allows_objects),
        ],
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

/// The network this suite's instance runs in: a VPC of its own, one subnet, and a route table
/// that subnet is associated with — which is the table a gateway endpoint is created for.
fn dedicated_network(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
) -> (String, String, String) {
    let created = ec2_ok(
        router,
        runtime,
        "the network",
        "CreateVpc",
        &[("CidrBlock", "10.92.0.0/16")],
    );
    let vpc = element(&created, "vpcId");
    let created = ec2_ok(
        router,
        runtime,
        "the subnet",
        "CreateSubnet",
        &[
            ("VpcId", &vpc),
            ("CidrBlock", "10.92.1.0/24"),
            ("AvailabilityZone", &format!("{REGION}a")),
        ],
    );
    let subnet = element(&created, "subnetId");
    let created = ec2_ok(
        router,
        runtime,
        "the route table",
        "CreateRouteTable",
        &[("VpcId", &vpc)],
    );
    let route_table = element(&created, "routeTableId");
    ec2_ok(
        router,
        runtime,
        "the association",
        "AssociateRouteTable",
        &[("RouteTableId", &route_table), ("SubnetId", &subnet)],
    );
    (vpc, subnet, route_table)
}

/// Create the S3 GATEWAY endpoint of `vpc` for `route_table` and look again until Amazon EC2
/// reports it `available`, which is the state it serves traffic in. `policy` is the document
/// attached to it, or `None` to take the full-access default the service gives one.
fn available_gateway_endpoint(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    vpc: &str,
    route_table: &str,
    policy: Option<&str>,
) -> String {
    let mut parameters = vec![
        ("VpcId", vpc),
        ("ServiceName", S3_ENDPOINT_SERVICE),
        ("VpcEndpointType", "Gateway"),
        ("RouteTableId.1", route_table),
    ];
    if let Some(policy) = policy {
        parameters.push(("PolicyDocument", policy));
    }
    let created = ec2_ok(
        router,
        runtime,
        "the endpoint",
        "CreateVpcEndpoint",
        &parameters,
    );
    let endpoint = element(&created, "vpcEndpointId");
    let until = Instant::now() + DEADLINE;
    loop {
        let described = ec2_ok(
            router,
            runtime,
            "the endpoint",
            "DescribeVpcEndpoints",
            &[("VpcEndpointId.1", &endpoint)],
        );
        if elements(&described, "state")
            .iter()
            .any(|state| state == ENDPOINT_AVAILABLE)
        {
            return endpoint;
        }
        assert!(
            Instant::now() < until,
            "setup: expected {endpoint} to reach {ENDPOINT_AVAILABLE}, last read {described}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Attach `policy` to the endpoint `endpoint`, the way a client narrows what its network may do
/// with the service.
fn attach_policy(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    endpoint: &str,
    policy: &str,
) {
    ec2_ok(
        router,
        runtime,
        case,
        "ModifyVpcEndpoint",
        &[("VpcEndpointId", endpoint), ("PolicyDocument", policy)],
    );
}

/// Launch one instance in `subnet`, running as `profile`, as the instance id Amazon EC2
/// answered with.
fn launch(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    image: &str,
    subnet: &str,
    profile: &str,
) -> String {
    let launched = ec2_ok(
        router,
        runtime,
        "the launch",
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
    instance_id: &str,
) {
    let until = Instant::now() + DEADLINE;
    loop {
        let described = ec2_ok(
            router,
            runtime,
            "the launch",
            "DescribeInstances",
            &[("InstanceId.1", instance_id)],
        );
        let state = block_of(&described, "instanceState").unwrap_or_default();
        if elements(state, "name").iter().any(|name| name == RUNNING) {
            break;
        }
        assert!(
            Instant::now() < until,
            "setup: expected Amazon EC2 to report {instance_id} {RUNNING}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
    loop {
        let reported = ssm_ok(
            router,
            runtime,
            "the node",
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
            "setup: expected {instance_id} to become a managed node reporting {ONLINE}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// The program the guest runs with the AWS CLI the machine's image ships — the OTHER form the
/// same script is written in: a shell line that invokes `aws s3api` and reports the status it
/// exited with, which is what a probe shipped to an in-VPC machine does.
///
/// It makes the same two calls through the same endpoint, a write of `key` whose bytes are a
/// file the script itself wrote and a read of the seeded object into another file of the
/// machine's storage, and reports each one's exit status. `set +e` is what lets the line after a
/// refused call still run, so the script reports BOTH.
fn cli_calls_script(key: &str) -> Vec<String> {
    vec![
        "set +e".to_string(),
        format!("printf '%s' '{WRITTEN_IN_THE_GUEST}' > {PAYLOAD_PATH}"),
        format!(
            "aws s3api put-object --bucket {BUCKET} --key {key} --body {PAYLOAD_PATH} \
             --region {REGION}"
        ),
        "echo PUTRC:$?".to_string(),
        format!(
            "aws s3api get-object --bucket {BUCKET} --key {SEEDED_KEY} --region {REGION} \
             {READ_PATH}"
        ),
        "echo GETRC:$?".to_string(),
        format!("echo READ:$(cat {READ_PATH})"),
    ]
}

/// The status one labelled line of the CLI script reports, read off what the invocation wrote.
fn exit_status(case: &str, report: &Value, label: &str) -> i64 {
    let output = report["StandardOutputContent"].as_str().unwrap_or_default();
    let prefix = format!("{label}:");
    output
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .and_then(|status| status.trim().parse::<i64>().ok())
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the script to report a {label} status, got {report:?}")
        })
}

/// Send `commands` to the guest of `instance_id` as one shell script, and look again until the
/// invocation has reached a terminal status — answering the whole `GetCommandInvocation`
/// report, which is what a client reads the program's two streams and its exit code out of.
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

/// The program the guest runs: the universal in-guest idiom — a shell script ending in an
/// inline Python program that uses the AWS SDK and prints one line per call for the client to
/// parse.
///
/// It makes TWO calls through the same endpoint, a write of `key` and a read of the seeded
/// object, and reports each one's outcome: whether the service served it, and the code and the
/// sentence of the refusal where it did not. Both are caught, so the invocation itself reports
/// what each call came to rather than the first failure.
fn object_calls_script(key: &str) -> Vec<String> {
    [
        "set -e",
        "python3 - <<'PY'",
        "import json",
        "import boto3",
        "client = boto3.client('s3')",
        "",
        "def outcome(label, call):",
        "    try:",
        "        call()",
        "        print(label + ':' + json.dumps({'served': True}))",
        "    except Exception as error:",
        "        answered = getattr(error, 'response', None) or dict()",
        "        detail = answered.get('Error') or dict()",
        "        print(label + ':' + json.dumps({",
        "            'served': False,",
        "            'code': detail.get('Code'),",
        "            'message': detail.get('Message'),",
        "        }))",
        "",
        "outcome('PUT', lambda: client.put_object(",
        &format!("    Bucket='{BUCKET}', Key='{key}', Body=b'written in the guest'))"),
        "outcome('GET', lambda: client.get_object(",
        &format!("    Bucket='{BUCKET}', Key='{SEEDED_KEY}'))"),
        "PY",
    ]
    .iter()
    .map(|line| (*line).to_string())
    .collect()
}

/// What the program reported for the call labelled `label`, parsed out of what the invocation
/// wrote to standard output.
fn reported(case: &str, report: &Value, label: &str) -> Value {
    let output = report["StandardOutputContent"].as_str().unwrap_or_default();
    let prefix = format!("{label}:");
    let line = output
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected the program to report a {label} outcome, got \
                 {report:?}"
            )
        });
    serde_json::from_str(line).unwrap_or_else(|error| {
        panic!("case {case:?}: expected a {label} outcome document, got {line:?}: {error}")
    })
}

/// Whether Amazon S3 holds an object under `key` — the backing a served write has and a refused
/// one must not.
fn object_is_held(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    key: &str,
) -> bool {
    object_text(router, runtime, key).is_some()
}

/// The bytes Amazon S3 holds under `key`, and `None` for a key it holds nothing under.
fn object_text(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    key: &str,
) -> Option<String> {
    let read = s3(router, runtime, "GET", &format!("/{BUCKET}/{key}"), "");
    (read.status == 200).then_some(read.text)
}

/// The lines one invocation wrote to standard output.
fn written_lines(report: &Value) -> Vec<String> {
    report["StandardOutputContent"]
        .as_str()
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// RULE: a call a program of the guest makes to the service that OWNS that guest is served
/// like any other — the store that holds the machine is not locked while the machine's own
/// script runs, so the call is answered inside the invocation rather than waiting for a guard
/// the script itself holds.
///
/// The row that matters is the one call that re-enters the service performing the script
/// (Amazon EC2's own `DescribeSecurityGroups`, which reads the store the guest's storage lives
/// in). A guest whose script asks for it must report what the account holds; the case asserts
/// the ANSWER rather than elapsed time, so it cannot pass on a call that was given up on.
#[test]
fn an_in_guest_call_to_the_service_that_owns_the_guest_is_served() {
    std::thread::Builder::new()
        .name("vpce-in-guest-reentrancy".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("a current-thread runtime");

            create_profile(&router, &runtime, "vpce-ec2-node", "vpce-ec2-node-role");
            let image = image_id(&router, &runtime);
            let (_vpc, subnet, _route_table) = dedicated_network(&router, &runtime);
            let instance = launch(&router, &runtime, &image, &subnet, "vpce-ec2-node");
            when_managed(&router, &runtime, &instance);

            let script: Vec<String> = [
                "set -e",
                "python3 - <<'PY'",
                "import json",
                "import boto3",
                "answered = boto3.client('ec2').describe_security_groups()",
                "groups = answered.get('SecurityGroups') or []",
                "print('GROUPS:' + json.dumps({'count': len(groups)}))",
                "PY",
            ]
            .iter()
            .map(|line| (*line).to_string())
            .collect();
            let report = ran_in_guest(
                &router,
                &runtime,
                "a call back into the service that owns the guest",
                &instance,
                &script,
            );
            assert_eq!(
                report["Status"].as_str(),
                Some(SUCCESS_STATUS),
                "the program must run to success, got {report}"
            );
            let answered = reported(
                "a call back into the service that owns the guest",
                &report,
                "GROUPS",
            );
            let count = answered["count"].as_i64().unwrap_or_default();
            assert!(
                count >= 1,
                "the in-guest call must be ANSWERED with the groups the account holds (its \
                 VPC's own default group at least), got {answered}"
            );
        })
        .expect("a sized thread")
        .join()
        .expect("the suite thread");
}

/// One row of the table: what the endpoint's policy says, and what the two calls through it
/// come to.
struct Row {
    /// What the case is about.
    case: &'static str,
    /// The document attached to the endpoint for this row.
    policy: String,
    /// The key the row's write addresses, so what each write left is read back on its own.
    key: &'static str,
    /// Whether the WRITE travels.
    write_served: bool,
    /// Whether the READ travels.
    read_served: bool,
    /// Where the write is refused, the clause the refusal states — which of the endpoint
    /// policy's readings withheld it.
    refusal_clause: Option<&'static str>,
}

/// The full-access document a client writes to open an endpoint to everything, which is also
/// what the service attaches to an endpoint created with none.
fn full_access() -> String {
    "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Allow\",\"Principal\":\"*\",\
     \"Action\":\"*\",\"Resource\":\"*\"}]}"
        .to_string()
}

/// Full access with one `Deny` of `action` on the objects of `bucket` after it, which is how an
/// account narrows one call of a service its network otherwise reaches.
fn denying(action: &str, bucket: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":\"*\",\"Action\":\"*\",\"Resource\":\"*\"}},{{\"Effect\":\"Deny\",\
         \"Principal\":\"*\",\"Action\":\"{action}\",\"Resource\":\"arn:aws:s3:::{bucket}/*\"}}]}}"
    )
}

/// A document that allows `actions` on the objects of [`BUCKET`] and states nothing else.
fn allowing(actions: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":\"*\",\"Action\":[{actions}],\
         \"Resource\":\"arn:aws:s3:::{BUCKET}/*\"}}]}}"
    )
}

#[test]
fn a_vpc_endpoint_policy_decides_an_sdk_call_made_inside_an_instance_guest() {
    std::thread::Builder::new()
        .name("vpce-policy-in-a-guest".to_string())
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

            create_profile(&router, &runtime, "vpce-node", "vpce-node-role");
            let image = image_id(&router, &runtime);
            let (vpc, subnet, route_table) = dedicated_network(&router, &runtime);
            create_bucket(&router, &runtime, BUCKET);
            create_bucket(&router, &runtime, OTHER_BUCKET);
            let seeded = s3(
                &router,
                &runtime,
                "PUT",
                &format!("/{BUCKET}/{SEEDED_KEY}"),
                SEEDED_CONTENT,
            );
            assert_eq!(
                seeded.status, 200,
                "setup: the seeded object must be stored, got {seeded:?}"
            );

            let instance = launch(&router, &runtime, &image, &subnet, "vpce-node");
            when_managed(&router, &runtime, &instance);

            // CONTROL, before any endpoint exists: the program's own call is a real call made
            // by the role the instance profile carries, so the object it writes is an object
            // Amazon S3 holds and the object it reads is the one the bucket holds.
            let report = ran_in_guest(
                &router,
                &runtime,
                "no endpoint at all",
                &instance,
                &object_calls_script("written/with-no-endpoint.txt"),
            );
            assert_eq!(
                report["Status"].as_str(),
                Some(SUCCESS_STATUS),
                "case \"no endpoint at all\": the program must run to success, got {report}"
            );
            let put = reported("no endpoint at all", &report, "PUT");
            assert_eq!(
                put["served"].as_bool(),
                Some(true),
                "case \"no endpoint at all\": the write must be served, got {put}"
            );
            assert!(
                object_is_held(&router, &runtime, "written/with-no-endpoint.txt"),
                "case \"no endpoint at all\": the write must leave an object the bucket holds"
            );
            let got = reported("no endpoint at all", &report, "GET");
            assert_eq!(
                got["served"].as_bool(),
                Some(true),
                "case \"no endpoint at all\": the read must be served, got {got}"
            );

            // The same control for the other program a script reaches Amazon S3 with: the AWS
            // CLI the machine's image ships, whose write carries a file the script itself put on
            // the machine's storage — so the object the bucket holds is those very bytes.
            let cli_key = "written/by-the-cli.txt";
            let report = ran_in_guest(
                &router,
                &runtime,
                "the command line with no endpoint at all",
                &instance,
                &cli_calls_script(cli_key),
            );
            assert_eq!(
                report["Status"].as_str(),
                Some(SUCCESS_STATUS),
                "case \"the command line with no endpoint at all\": the script must run to \
                 success, got {report}"
            );
            for label in ["PUTRC", "GETRC"] {
                let status = exit_status("the command line with no endpoint at all", &report, label);
                assert_eq!(
                    status, CLI_SUCCEEDED,
                    "case \"the command line with no endpoint at all\": expected the CLI's \
                     {label} to be {CLI_SUCCEEDED}, got {status}"
                );
            }
            assert_eq!(
                object_text(&router, &runtime, cli_key).as_deref(),
                Some(WRITTEN_IN_THE_GUEST),
                "the CLI's write must leave the bytes the script put on the machine"
            );
            let output = written_lines(&report);
            assert!(
                output
                    .iter()
                    .any(|line| line == &format!("READ:{SEEDED_CONTENT}")),
                "the CLI's read must leave the object's bytes on the machine, got {output:?}"
            );

            // The endpoint itself, created with no document of its own: what it is given is the
            // full-access default, so nothing the account may do is narrowed by it.
            let endpoint =
                available_gateway_endpoint(&router, &runtime, &vpc, &route_table, None);

            let rows = vec![
                Row {
                    case: "the full-access default",
                    policy: full_access(),
                    key: "written/under-full-access.txt",
                    write_served: true,
                    read_served: true,
                    refusal_clause: None,
                },
                Row {
                    case: "a deny of the write on this bucket",
                    policy: denying("s3:PutObject", BUCKET),
                    key: "written/under-a-deny.txt",
                    write_served: false,
                    read_served: true,
                    refusal_clause: Some("with an explicit deny in a VPC endpoint policy"),
                },
                Row {
                    case: "a document allowing reads alone",
                    policy: allowing("\"s3:GetObject\""),
                    key: "written/under-reads-only.txt",
                    write_served: false,
                    read_served: true,
                    refusal_clause: Some(
                        "because no VPC endpoint policy allows the s3:PutObject action",
                    ),
                },
                Row {
                    case: "a deny scoped to another bucket",
                    policy: denying("s3:PutObject", OTHER_BUCKET),
                    key: "written/past-another-buckets-deny.txt",
                    write_served: true,
                    read_served: true,
                    refusal_clause: None,
                },
                Row {
                    case: "a document allowing both calls",
                    policy: allowing("\"s3:PutObject\",\"s3:GetObject\""),
                    key: "written/under-both-allowed.txt",
                    write_served: true,
                    read_served: true,
                    refusal_clause: None,
                },
                Row {
                    case: "a document allowing writes alone",
                    policy: allowing("\"s3:PutObject\""),
                    key: "written/under-writes-only.txt",
                    write_served: true,
                    read_served: false,
                    refusal_clause: None,
                },
            ];

            for row in rows {
                let case = row.case;
                attach_policy(&router, &runtime, case, &endpoint, &row.policy);
                let report = ran_in_guest(
                    &router,
                    &runtime,
                    case,
                    &instance,
                    &object_calls_script(row.key),
                );
                assert_eq!(
                    report["Status"].as_str(),
                    Some(SUCCESS_STATUS),
                    "case {case:?}: the program itself must run to success, got {report}"
                );
                for (label, served) in [("PUT", row.write_served), ("GET", row.read_served)] {
                    let outcome = reported(case, &report, label);
                    assert_eq!(
                        outcome["served"].as_bool(),
                        Some(served),
                        "case {case:?}: expected the {label} call served={served}, got {outcome}"
                    );
                    if served {
                        continue;
                    }
                    // A call the endpoint policy did not carry is refused the way the called
                    // service refuses an unauthorized request, and the sentence says which
                    // document withheld it and what it withheld.
                    assert_eq!(
                        outcome["code"].as_str(),
                        Some(ACCESS_DENIED),
                        "case {case:?}: expected the {label} refusal's code, got {outcome}"
                    );
                    let message = outcome["message"].as_str().unwrap_or_default();
                    assert!(
                        message.contains("assumed-role/vpce-node-role/")
                            && message.contains(&format!("arn:aws:s3:::{BUCKET}/")),
                        "case {case:?}: expected the {label} refusal to name the session and the \
                         resource, got {message:?}"
                    );
                }
                if let Some(clause) = row.refusal_clause {
                    let message = reported(case, &report, "PUT")["message"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    assert!(
                        message.contains(clause),
                        "case {case:?}: expected the refusal to state {clause:?}, got {message:?}"
                    );
                }
                // The write leaves an object exactly where it travelled: a refused call writes
                // nothing, which is what makes the decision observable in the bucket itself.
                assert_eq!(
                    object_is_held(&router, &runtime, row.key),
                    row.write_served,
                    "case {case:?}: expected the bucket to hold {:?}={}",
                    row.key,
                    row.write_served
                );

                // The SAME two calls through the same endpoint, made by the other program a
                // script reaches Amazon S3 with: the AWS CLI the machine's image ships, whose
                // write carries a file the script itself put on the machine's storage. One
                // endpoint policy decides a call whichever program makes it.
                let cli_key = format!("{}.cli", row.key);
                let report = ran_in_guest(
                    &router,
                    &runtime,
                    case,
                    &instance,
                    &cli_calls_script(&cli_key),
                );
                assert_eq!(
                    report["Status"].as_str(),
                    Some(SUCCESS_STATUS),
                    "case {case:?}: the script itself must run to success, got {report}"
                );
                let written = report["StandardErrorContent"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                for (label, served) in [("PUTRC", row.write_served), ("GETRC", row.read_served)] {
                    let status = exit_status(case, &report, label);
                    let expected = if served {
                        CLI_SUCCEEDED
                    } else {
                        CLI_SERVICE_ERROR
                    };
                    assert_eq!(
                        status, expected,
                        "case {case:?}: expected the CLI's {label} to be {expected}, got {status}"
                    );
                }
                if row.write_served && row.read_served {
                    let output = written_lines(&report);
                    assert!(
                        output
                            .iter()
                            .any(|line| line == &format!("READ:{SEEDED_CONTENT}")),
                        "case {case:?}: the CLI's read must leave the object's bytes on the \
                         machine, got {output:?}"
                    );
                } else {
                    // The CLI reports a refused call the way it reports any service refusal: the
                    // code and the sentence the request came back with, on its standard error.
                    assert!(
                        written.contains(&format!("({ACCESS_DENIED})"))
                            && written.contains("assumed-role/vpce-node-role/"),
                        "case {case:?}: expected the CLI to report the refusal, got {written:?}"
                    );
                }
                if let Some(clause) = row.refusal_clause {
                    assert!(
                        written.contains(clause),
                        "case {case:?}: expected the CLI's refusal to state {clause:?}, got \
                         {written:?}"
                    );
                }
                assert_eq!(
                    object_is_held(&router, &runtime, &cli_key),
                    row.write_served,
                    "case {case:?}: expected the bucket to hold {cli_key:?}={}",
                    row.write_served
                );
            }
        })
        .expect("a sized thread")
        .join()
        .expect("the suite thread");
}
