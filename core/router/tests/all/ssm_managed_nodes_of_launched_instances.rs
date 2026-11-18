// SPDX-License-Identifier: CC0-1.0

//! Which of the account's Amazon EC2 instances Systems Manager reports as MANAGED NODES, on
//! the whole assembled emulator a client talks to.
//!
//! RULE under test: an instance is a managed node exactly when the three components that own
//! the evidence say so — Amazon EC2 holds the instance running, it holds an association
//! between that instance and an IAM instance profile, and IAM holds that profile carrying a
//! role Amazon EC2 may act as whose identity-based policies let the agent perform the call it
//! registers with. A span after the launch, whether or not anybody called in between,
//! `DescribeInstanceInformation` reports one `InstanceInformation` for such an instance with
//! `PingStatus` `Online` (`smithy_query(query="shape", shape="PingStatus")`:
//! `Online | ConnectionLost | Inactive`), `ResourceType` `EC2Instance`
//! (`ResourceType`: `ManagedInstance | EC2Instance`), a `LastPingDateTime`, the
//! `PlatformType` of the image it was launched from and the NAME of that role as `IamRole`.
//! An instance that runs as no such profile never registers, so a query narrowed to its id
//! answers an empty `InstanceInformationList` — while Amazon EC2 goes on reporting that very
//! instance running, which is what makes the absence a contract rather than a blanket one.
//!
//! This suite drives the ASSEMBLED router — every service mounted on one engine, as
//! `core/router/src/lib.rs` assembles it — because that is the emulator a client reaches,
//! and because the registration is derived across three of those services: a client resolves
//! its image id through Systems Manager's own public parameter, builds the role and the
//! instance profile through IAM, launches through Amazon EC2 and then asks Systems Manager
//! about the node. Nothing reaches into a state struct; every fact is written and read
//! through the service that owns it. Assembling the whole router costs stack (see
//! `boot_serve.rs`), so it is built — and the requests driven — on a sized thread.
//!
//! The rows differ along what the rule generalises over: how the role is GRANTED the
//! registration call (the AWS-managed policy a client attaches for this, an inline statement
//! naming the action) and how it is WITHHELD (no instance profile at all, a role whose
//! policies allow nothing). The FULLER table over those dimensions — an explicit deny, a
//! wildcard grant, a role another service may act as, a profile IAM holds no record of, a
//! Windows image, an instance that stops being held running — is the owning crate's own
//! (`services/service-com-amazonaws-ssm-amazonssm/tests/all/ec2_managed_node_registration.rs`),
//! and is not repeated here: what this suite adds beside it is the ASSEMBLED emulator, where
//! each of those calls has to route to the service that owns it, together with the negative
//! contract read at the one moment that makes it a contract — the sibling is absent from the
//! managed-node report WHILE Amazon EC2 reports that same instance running.
//!
//! Nothing here waits a fixed time: both spans this suite crosses — Amazon EC2's own move to
//! `running` and the registration — are stored due instants, so a case LOOKS AGAIN until the
//! condition holds or [`DEADLINE`] passes, and no assertion is made about elapsed time. The
//! rows that must NOT register are launched BEFORE the rows that must, so that by the time
//! the last registration has been observed every earlier instance's own span has passed too.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The form encoder and the XML readers an awsQuery/ec2Query suite needs live ONCE in the
// test crate, in `query_wire`.
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

/// The AWS-managed policy a client attaches to an instance profile's role to let the agent
/// register. Its document is AWS's own rather than the account's.
const SSM_CORE_POLICY: &str = "arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore";

/// An inline document granting exactly the call the agent registers its node with.
const GRANTS_REGISTRATION: &str = "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\
    \"Allow\",\"Action\":\"ssm:UpdateInstanceInformation\",\"Resource\":\"*\"}]}";

/// The service principal a role has to admit for Amazon EC2 to act as it on an instance's
/// behalf, so the agent on that instance has credentials at all.
const EC2_PRINCIPAL: &str = "ec2.amazonaws.com";

/// The `PingStatus` of a node whose agent is responding.
const ONLINE: &str = "Online";

/// The `ResourceType` of a node that is an Amazon EC2 instance.
const RESOURCE_TYPE_EC2: &str = "EC2Instance";

/// The `PlatformType` of a node launched from an image that declares no Windows platform.
const PLATFORM_LINUX: &str = "Linux";

/// The instance state the `instance_running` waiter of a client waits for before it asks
/// Systems Manager anything.
const RUNNING: &str = "running";

/// How long a case is given for a condition another component's clock owes.
///
/// Generous on purpose: each span is a stored due instant and nothing sleeps for one, so a
/// case looks again until the condition holds. All this bound is for is that a move that
/// never comes fails instead of hanging; nothing asserts on elapsed time.
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
    /// The HTTP status.
    status: u16,
    /// The body as the service wrote it, which is what an XML wire is read from.
    text: String,
    /// The same body parsed, which is meaningful for a JSON wire alone: an ec2Query or
    /// awsQuery answer is XML, so this is `Value::Null` for one and nothing reads it.
    body: Value,
}

/// One request at `signing_name`'s endpoint in [`REGION`].
fn send(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    signing_name: &str,
    content_type: &str,
    target: Option<String>,
    body: String,
) -> Answer {
    let mut builder = http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.{REGION}.amazonaws.com/"))
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

/// One ec2Query action of Amazon EC2.
fn ec2(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    action: &str,
    parameters: &[(&str, &str)],
) -> Answer {
    send(
        router,
        runtime,
        "ec2",
        "application/x-www-form-urlencoded",
        None,
        form_body(action, EC2_API_VERSION, parameters),
    )
}

/// One ec2Query action that must be served, as its XML body.
fn ec2_ok(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    action: &str,
    parameters: &[(&str, &str)],
) -> String {
    let answer = ec2(router, runtime, action, parameters);
    assert_eq!(
        answer.status, 200,
        "case {case:?}: Amazon EC2 {action} must be served, got {answer:?}"
    );
    answer.text
}

/// One awsQuery action of IAM that must be served, as its XML body.
fn iam_ok(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    action: &str,
    parameters: &[(&str, &str)],
) -> String {
    let answer = send(
        router,
        runtime,
        "iam",
        "application/x-www-form-urlencoded",
        None,
        form_body(action, IAM_API_VERSION, parameters),
    );
    assert_eq!(
        answer.status, 200,
        "case {case:?}: IAM {action} must be served, got {answer:?}"
    );
    answer.text
}

/// One Systems Manager operation over its awsJson1_1 wire.
fn ssm(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    operation: &str,
    body: &Value,
) -> Answer {
    send(
        router,
        runtime,
        "ssm",
        "application/x-amz-json-1.1",
        Some(format!("AmazonSSM.{operation}")),
        body.to_string(),
    )
}

/// One Systems Manager operation that must be served, as its parsed body.
fn ssm_ok(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let answer = ssm(router, runtime, operation, body);
    assert_eq!(
        answer.status, 200,
        "case {case:?}: Systems Manager {operation} must be served, got {answer:?}"
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

/// What one instance profile's role is granted — or withheld — about the registration call.
#[derive(Debug, Clone, Copy)]
enum Grant {
    /// The AWS-managed policy a client attaches for this.
    AwsManaged,
    /// An inline document of the account's own.
    Inline(&'static str),
    /// No policy at all.
    Nothing,
}

/// One instance profile a row launches an instance with.
#[derive(Debug, Clone, Copy)]
struct Profile {
    /// The name of the profile.
    name: &'static str,
    /// The name of the one role it carries, deliberately NOT the profile's own name: the
    /// rule says a node reports the ROLE's name as its `IamRole`, so a report carrying the
    /// profile's name instead has to be a failure rather than a coincidence.
    role: &'static str,
    /// The service principal its role's trust policy admits.
    trusts: &'static str,
    /// What its role's identity-based policies say about the registration call.
    grant: Grant,
}

/// Build one instance profile the way a client builds one — `CreateRole`, the grant,
/// `CreateInstanceProfile`, `AddRoleToInstanceProfile` — and read it back through IAM's own
/// reader, so the profile an instance is launched with is one IAM really holds.
fn create_profile(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    profile: &Profile,
) {
    let case = profile.name;
    let trust = trust_policy(profile.trusts);
    iam_ok(
        router,
        runtime,
        case,
        "CreateRole",
        &[
            ("RoleName", profile.role),
            ("AssumeRolePolicyDocument", &trust),
        ],
    );
    match profile.grant {
        Grant::AwsManaged => {
            iam_ok(
                router,
                runtime,
                case,
                "AttachRolePolicy",
                &[("RoleName", profile.role), ("PolicyArn", SSM_CORE_POLICY)],
            );
        }
        Grant::Inline(document) => {
            iam_ok(
                router,
                runtime,
                case,
                "PutRolePolicy",
                &[
                    ("RoleName", profile.role),
                    ("PolicyName", "registration"),
                    ("PolicyDocument", document),
                ],
            );
        }
        Grant::Nothing => {}
    }
    iam_ok(
        router,
        runtime,
        case,
        "CreateInstanceProfile",
        &[("InstanceProfileName", profile.name)],
    );
    iam_ok(
        router,
        runtime,
        case,
        "AddRoleToInstanceProfile",
        &[
            ("InstanceProfileName", profile.name),
            ("RoleName", profile.role),
        ],
    );
    let read = iam_ok(
        router,
        runtime,
        case,
        "GetInstanceProfile",
        &[("InstanceProfileName", profile.name)],
    );
    assert!(
        read.contains(&format!("<RoleName>{}</RoleName>", profile.role)),
        "case {case:?}: the profile IAM holds must carry its role, got {read}"
    );
}

/// The image id the public Systems Manager parameter `name` resolves to, read through that
/// service's own `GetParameter` — the call a client resolves its AMI with.
fn image_id(router: &MultiServiceRouter, runtime: &tokio::runtime::Runtime, name: &str) -> String {
    let answered = ssm_ok(
        router,
        runtime,
        name,
        "GetParameter",
        &json!({ "Name": name }),
    );
    answered["Parameter"]["Value"]
        .as_str()
        .unwrap_or_else(|| panic!("case {name:?}: GetParameter must report a value, got {answered}"))
        .to_string()
}

/// Launch one instance through Amazon EC2's own `RunInstances`, optionally running as the
/// instance profile `profile`, as the instance id that service answered.
fn launch(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    image: &str,
    profile: Option<&str>,
) -> String {
    let mut parameters = vec![("ImageId", image), ("MinCount", "1"), ("MaxCount", "1")];
    if let Some(profile) = profile {
        parameters.push(("IamInstanceProfile.Name", profile));
    }
    let launched = ec2_ok(router, runtime, case, "RunInstances", &parameters);
    element(&launched, "instanceId")
}

/// Whether Amazon EC2 reports `instance_id` in the [`RUNNING`] state, read out of that
/// instance's own state element.
fn is_running(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    instance_id: &str,
) -> bool {
    let described = ec2_ok(
        router,
        runtime,
        case,
        "DescribeInstances",
        &[("InstanceId.1", instance_id)],
    );
    let state = block_of(&described, "instanceState").unwrap_or_default();
    elements(state, "name").iter().any(|name| name == RUNNING)
}

/// Look again until Amazon EC2 reports `instance_id` running, which is the condition the
/// `instance_running` waiter of a client waits for.
fn when_running(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    instance_id: &str,
) {
    let until = Instant::now() + DEADLINE;
    while !is_running(router, runtime, case, instance_id) {
        assert!(
            Instant::now() < until,
            "case {case:?}: expected Amazon EC2 to report {instance_id} {RUNNING}, and it \
             reported another state"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// The `InstanceInformation` Systems Manager reports for `instance_id`, or `None` when it
/// reports none — read through the filter a client narrows the report with.
fn node(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    instance_id: &str,
) -> Option<Value> {
    let reported = ssm_ok(
        router,
        runtime,
        case,
        "DescribeInstanceInformation",
        &json!({ "Filters": [{ "Key": "InstanceIds", "Values": [instance_id] }] }),
    );
    let listed = reported["InstanceInformationList"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        listed.len() <= 1,
        "case {case:?}: a query narrowed to {instance_id} reports at most one node, got \
         {reported}"
    );
    listed.first().cloned()
}

/// Look again until Systems Manager reports `instance_id` as a managed node.
fn node_when_registered(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    instance_id: &str,
) -> Value {
    let until = Instant::now() + DEADLINE;
    loop {
        if let Some(reported) = node(router, runtime, case, instance_id) {
            return reported;
        }
        assert!(
            Instant::now() < until,
            "case {case:?}: expected {instance_id} to become a managed node, and \
             DescribeInstanceInformation reported none"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Every managed-node id Systems Manager reports when nothing narrows the report.
fn every_node(router: &MultiServiceRouter, runtime: &tokio::runtime::Runtime) -> Vec<String> {
    let reported = ssm_ok(
        router,
        runtime,
        "the whole report",
        "DescribeInstanceInformation",
        &json!({}),
    );
    reported["InstanceInformationList"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|node| node["InstanceId"].as_str().map(str::to_string))
        .collect()
}

/// The profiles the rows below are launched with.
fn profiles() -> Vec<Profile> {
    vec![
        Profile {
            name: "node-core",
            role: "node-core-role",
            trusts: EC2_PRINCIPAL,
            grant: Grant::AwsManaged,
        },
        Profile {
            name: "node-inline",
            role: "node-inline-role",
            trusts: EC2_PRINCIPAL,
            grant: Grant::Inline(GRANTS_REGISTRATION),
        },
        Profile {
            name: "node-no-grant",
            role: "node-no-grant-role",
            trusts: EC2_PRINCIPAL,
            grant: Grant::Nothing,
        },
    ]
}

/// What the rule says about one row's instance.
///
/// ONE field, so a row cannot say both: a row that registers names the role the node has to
/// report as its `IamRole`, and there is nothing else to say about one that does not.
#[derive(Debug, Clone, Copy)]
enum Expected {
    /// The instance becomes a managed node reporting this role name.
    Registers(&'static str),
    /// The instance becomes no managed node at all.
    DoesNotRegister,
}

/// One row of the table: an instance launched a given way, and what the rule says about it.
#[derive(Debug, Clone, Copy)]
struct Case {
    case: &'static str,
    /// The instance profile to launch with, or `None` to launch with none at all.
    profile: Option<&'static str>,
    /// Whether the instance must become a managed node, and as which role.
    expected: Expected,
}

/// The table, with every row that must NOT register before every row that must: the spans
/// are per instance, so observing the last registration proves every earlier instance's own
/// span has passed.
fn rows() -> Vec<Case> {
    vec![
        Case {
            case: "an instance running as no instance profile",
            profile: None,
            expected: Expected::DoesNotRegister,
        },
        Case {
            case: "a role whose policies allow nothing",
            profile: Some("node-no-grant"),
            expected: Expected::DoesNotRegister,
        },
        Case {
            case: "a role carrying the AWS-managed policy a client attaches for this",
            profile: Some("node-core"),
            expected: Expected::Registers("node-core-role"),
        },
        Case {
            case: "a role whose inline policy names the registration call",
            profile: Some("node-inline"),
            expected: Expected::Registers("node-inline-role"),
        },
    ]
}

#[test]
fn systems_manager_reports_a_node_for_each_instance_whose_profile_lets_the_agent_register() {
    std::thread::Builder::new()
        .name("ssm-managed-nodes".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            // Signatures are not what this suite is about; the router still reads the SigV4
            // credential scope, which is where each call's Region comes from.
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a current-thread runtime");

            for profile in profiles() {
                create_profile(&router, &runtime, &profile);
            }
            let image = image_id(&router, &runtime, AL2023_PARAMETER);

            // Launch every row first, so the registration spans run side by side.
            let table = rows();
            let launched: Vec<String> = table
                .iter()
                .map(|row| launch(&router, &runtime, row.case, &image, row.profile))
                .collect();

            // Then wait, as a client's `instance_running` waiter does, for Amazon EC2 to
            // hold each instance running — the state a node is reported in.
            for (row, instance) in table.iter().zip(&launched) {
                when_running(&router, &runtime, row.case, instance);
            }

            // The rows that register, each reporting the instance's own facts.
            for (row, instance) in table.iter().zip(&launched) {
                let Expected::Registers(iam_role) = row.expected else {
                    continue;
                };
                let reported = node_when_registered(&router, &runtime, row.case, instance);
                assert_eq!(
                    reported["InstanceId"].as_str(),
                    Some(instance.as_str()),
                    "case {:?}: expected the node of {instance}, got {reported}",
                    row.case
                );
                assert_eq!(
                    reported["PingStatus"].as_str(),
                    Some(ONLINE),
                    "case {:?}: expected PingStatus {ONLINE}, got {reported}",
                    row.case
                );
                assert_eq!(
                    reported["ResourceType"].as_str(),
                    Some(RESOURCE_TYPE_EC2),
                    "case {:?}: expected ResourceType {RESOURCE_TYPE_EC2}, got {reported}",
                    row.case
                );
                assert_eq!(
                    reported["PlatformType"].as_str(),
                    Some(PLATFORM_LINUX),
                    "case {:?}: expected PlatformType {PLATFORM_LINUX}, got {reported}",
                    row.case
                );
                assert_eq!(
                    reported["IamRole"].as_str(),
                    Some(iam_role),
                    "case {:?}: expected IamRole {iam_role:?}, got {reported}",
                    row.case
                );
                let last_ping = reported["LastPingDateTime"].as_f64();
                assert!(
                    last_ping.is_some_and(|seconds| seconds > 0.0),
                    "case {:?}: expected a LastPingDateTime, got {reported}",
                    row.case
                );
            }

            // Every row that must not register: no node, while Amazon EC2 goes on reporting
            // that same instance running. Each of these was launched before the rows above,
            // so its own span has passed by now.
            for (row, instance) in table.iter().zip(&launched) {
                if matches!(row.expected, Expected::Registers(_)) {
                    continue;
                }
                let reported = node(&router, &runtime, row.case, instance);
                assert_eq!(
                    reported, None,
                    "case {:?}: expected no managed node for {instance}, got {reported:?}",
                    row.case
                );
                assert!(
                    is_running(&router, &runtime, row.case, instance),
                    "case {:?}: expected Amazon EC2 to still report {instance} {RUNNING}",
                    row.case
                );
            }

            // The unnarrowed report agrees with the narrowed ones: it carries exactly the
            // rows that registered.
            let reported = every_node(&router, &runtime);
            for (row, instance) in table.iter().zip(&launched) {
                let registers = matches!(row.expected, Expected::Registers(_));
                assert_eq!(
                    reported.contains(instance),
                    registers,
                    "case {:?}: expected the whole report to carry {instance} = {registers}, got \
                     {reported:?}",
                    row.case
                );
            }
        })
        .expect("a sized thread")
        .join()
        .expect("the suite thread");
}
