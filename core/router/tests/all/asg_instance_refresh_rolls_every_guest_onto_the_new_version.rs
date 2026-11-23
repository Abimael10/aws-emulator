// SPDX-License-Identifier: CC0-1.0

//! An INSTANCE REFRESH rolling a new launch-template generation out across every guest of an
//! Auto Scaling group, read the way a client that rolls one out reads it: on the whole
//! assembled emulator, through the control plane and then from inside each replacement's own
//! guest.
//!
//! RULE under test: an instance refresh REPLACES every instance the group held when it
//! started with instances launched from the launch-template version the group's launch source
//! then resolves to — "Rolling - Terminates instances and launches replacements in batches"
//! (`smithy_query(query="doc",shape="InstanceRefresh",member="Strategy")`) — so once
//! `DescribeInstanceRefreshes` reports a terminal status
//! (`smithy_query(query="doc",shape="InstanceRefresh",member="Status")`: `Successful`,
//! `Failed`, `Cancelled`, `RollbackSuccessful` or `RollbackFailed`) the refresh is
//! `Successful`, every pre-refresh id is gone from the group and reported `terminated` by
//! Amazon EC2, and the group holds its desired capacity in instances whose ids are all new.
//! Each of those replacements is a REAL guest of that version: its agent registers it as a
//! managed node, and a Run Command invocation in it reports its own instance id from the
//! instance metadata service and the generation marker only the NEW version's user data
//! writes — so a client can tell which generation each running guest is on.
//!
//! The rows differ along what the rule generalises over: how many guests the group holds (one,
//! two), HOW its launch source comes to resolve the new version (a numbered version the group
//! is updated to, the `$Latest` word it floats on, the `$Default` word plus a default-version
//! change), and the FORM the version's user data takes (a user-data script, a
//! `#cloud-config` document's `write_files`). The marker each generation writes is the row's
//! own, so a guest reporting another row's marker — or its own earlier generation's — is a
//! failure rather than a coincidence.
//!
//! This suite drives the ASSEMBLED router — every service mounted on one engine, as
//! `core/router/src/lib.rs` assembles it — because the chain crosses four of them: a client
//! resolves the image id through Systems Manager's own public parameter, builds the role and
//! instance profile through IAM, builds the network and the launch-template generations
//! through Amazon EC2, refreshes through Amazon EC2 Auto Scaling, and then asks Systems
//! Manager to run a command inside each guest. Nothing reaches into a state struct: every
//! fact is written and read through the service that owns it. Assembling the whole router
//! costs stack (see `boot_serve.rs`), so it is built — and the requests driven — on a sized
//! thread.
//!
//! Nothing here waits a fixed time: every span crossed (Amazon EC2's move to `running`, the
//! node's registration, each replacement the refresh makes, the invocation's move off
//! `Pending`) is a stored due instant, so a case LOOKS AGAIN until the condition holds or
//! [`DEADLINE`] passes, and no assertion is made about elapsed time. The rows are carried
//! through each phase TOGETHER, so their spans run side by side rather than one after
//! another.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The form encoder and the XML readers an awsQuery/ec2Query suite needs live ONCE in the test
// crate, in `query_wire`.
use crate::query_wire::{block_of, element, elements, form_encode, members_of};

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every request below is signed for, and the zone every group places in.
const REGION: &str = "us-east-1";
const ZONE: &str = "us-east-1a";

/// The API versions an ec2Query, an awsQuery for IAM and an awsQuery for Auto Scaling carry.
const EC2_API_VERSION: &str = "2016-11-15";
const IAM_API_VERSION: &str = "2010-05-08";
const ASG_API_VERSION: &str = "2011-01-01";

/// The public Systems Manager parameter a client resolves the Amazon Linux 2023 image id
/// through, which is the image every generation below launches from.
const AL2023_PARAMETER: &str =
    "/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64";

/// The instance type every launch-template generation names.
const INSTANCE_TYPE: &str = "t3.micro";

/// The AWS-managed policy a client attaches to the instance profile's role so the agent in
/// each guest registers its node and takes the commands sent to it.
const SSM_CORE_POLICY: &str = "arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore";

/// The service principal a role has to admit for Amazon EC2 to act as it on an instance's
/// behalf, so the agent in the guest has credentials at all.
const EC2_PRINCIPAL: &str = "ec2.amazonaws.com";

/// The document Run Command runs a shell script on a Linux node with.
const SHELL_DOCUMENT: &str = "AWS-RunShellScript";

/// Where every generation's user data leaves its marker, and the name the probe reads it by.
const GENERATION_PATH: &str = "/etc/launch-template-generation";

/// The lifecycle state and the health a group reports for an instance that is serving
/// (`AutoScalingInstanceDetails$LifecycleState`, `$HealthStatus`).
const IN_SERVICE: &str = "InService";
const HEALTHY: &str = "Healthy";

/// The `InstanceStateName` Amazon EC2 reports for an instance the refresh took out.
const TERMINATED: &str = "terminated";

/// The `PingStatus` of a node whose agent is responding.
const ONLINE: &str = "Online";

/// The statuses an instance refresh has ENDED in, from the status member's own documentation
/// (`smithy_query(query="doc",shape="InstanceRefresh",member="Status")`): the five of the
/// modelled `InstanceRefreshStatus` enum that are not a stage on the way somewhere.
const TERMINAL_REFRESH_STATUSES: &[&str] = &[
    "Successful",
    "Failed",
    "Cancelled",
    "RollbackSuccessful",
    "RollbackFailed",
];

/// The one terminal status a refresh that replaced every instance it set out to reaches:
/// "`Successful` - An instance refresh completed successfully".
const SUCCESSFUL: &str = "Successful";

/// The statuses a Run Command invocation has ended in
/// (`smithy_query(query="shape",shape="CommandInvocationStatus")`).
const TERMINAL_INVOCATION_STATUSES: &[&str] = &["Success", "Cancelled", "TimedOut", "Failed"];

/// The status of an invocation whose script ran and ended cleanly.
const INVOCATION_SUCCESS: &str = "Success";

/// How long a case is given for a condition another component's clock owes.
///
/// Generous on purpose: each span is a stored due instant and nothing sleeps for one, so a
/// case looks again until the condition holds. All this bound is for is that a move that never
/// comes fails instead of hanging; nothing asserts on elapsed time.
const DEADLINE: Duration = Duration::from_secs(180);

/// How long one look-again cycle pauses before asking again. Not a bound on anything.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// One answer, reduced to what a client branches on.
#[derive(Debug)]
struct Answer {
    /// The HTTP status.
    status: u16,
    /// The body as the service wrote it, which is what an XML wire is read from.
    text: String,
    /// The same body parsed, meaningful for a JSON wire alone.
    body: Value,
}

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// One `LaunchTemplateData.<member>` parameter of a launch-template write.
fn data(member: &str, value: impl Into<String>) -> (String, String) {
    p(&format!("LaunchTemplateData.{member}"), value)
}

/// The whole assembled emulator, and the runtime its requests are driven on.
struct Emulator {
    router: MultiServiceRouter,
    runtime: tokio::runtime::Runtime,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
        let mut router = build_router(engine);
        // Signatures are not what this suite is about; the router still reads the SigV4
        // credential scope, which is where each call's Region comes from.
        router.set_request_authentication(false);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("a current-thread runtime");
        Self { router, runtime }
    }

    /// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is
    /// what routes the request and where its Region comes from; the signature is never
    /// checked.
    fn authorization(signing_name: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
             aws4_request, SignedHeaders=host;x-amz-date, Signature=deadbeef"
        )
    }

    /// One request at `signing_name`'s endpoint in [`REGION`].
    fn send(
        &self,
        signing_name: &str,
        content_type: &str,
        target: Option<String>,
        body: String,
    ) -> Answer {
        let mut builder = http::Request::builder()
            .method("POST")
            .uri(format!("https://{signing_name}.{REGION}.amazonaws.com/"))
            .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
            .header("authorization", Self::authorization(signing_name))
            .header("content-type", content_type);
        if let Some(target) = target {
            builder = builder.header("x-amz-target", target);
        }
        let request = builder
            .body(bytes::Bytes::from(body))
            .expect("a well-formed request");
        let response = self.runtime.block_on(self.router.dispatch(request));
        let status = response.status().as_u16();
        let text = String::from_utf8_lossy(response.body()).to_string();
        let body = serde_json::from_str(&text).unwrap_or(Value::Null);
        Answer { status, text, body }
    }

    /// One form-encoded action that must be served, as its XML body.
    fn form_ok(
        &self,
        signing_name: &str,
        version: &str,
        case: &str,
        action: &str,
        parameters: &[(String, String)],
    ) -> String {
        let mut body = format!("Action={action}&Version={version}");
        for (name, value) in parameters {
            body.push('&');
            body.push_str(&form_encode(name));
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let answer = self.send(
            signing_name,
            "application/x-www-form-urlencoded",
            None,
            body,
        );
        assert_eq!(
            answer.status, 200,
            "case {case:?}: {signing_name} {action} must be served, got {answer:?}"
        );
        answer.text
    }

    /// One ec2Query action of Amazon EC2 that must be served.
    fn ec2_ok(&self, case: &str, action: &str, parameters: &[(String, String)]) -> String {
        self.form_ok("ec2", EC2_API_VERSION, case, action, parameters)
    }

    /// One awsQuery action of IAM that must be served.
    fn iam_ok(&self, case: &str, action: &str, parameters: &[(String, String)]) -> String {
        self.form_ok("iam", IAM_API_VERSION, case, action, parameters)
    }

    /// One awsQuery action of Amazon EC2 Auto Scaling that must be served.
    fn asg_ok(&self, case: &str, action: &str, parameters: &[(String, String)]) -> String {
        self.form_ok("autoscaling", ASG_API_VERSION, case, action, parameters)
    }

    /// One Systems Manager operation over its awsJson1_1 wire that must be served, as its
    /// parsed body.
    fn ssm_ok(&self, case: &str, operation: &str, body: &Value) -> Value {
        let answer = self.send(
            "ssm",
            "application/x-amz-json-1.1",
            Some(format!("AmazonSSM.{operation}")),
            body.to_string(),
        );
        assert_eq!(
            answer.status, 200,
            "case {case:?}: Systems Manager {operation} must be served, got {answer:?}"
        );
        answer.body
    }
}

/// A trust policy admitting `principal` and nothing else.
fn trust_policy(principal: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"Service\":\"{principal}\"}},\"Action\":\"sts:AssumeRole\"}}]}}"
    )
}

/// Build the instance profile every group's instances run as, the way a client builds one, so
/// the agent in each guest registers its node and takes the commands sent to it.
fn create_profile(emulator: &Emulator, profile: &str, role: &str) {
    let case = "the instance profile";
    emulator.iam_ok(
        case,
        "CreateRole",
        &[
            p("RoleName", role),
            p("AssumeRolePolicyDocument", trust_policy(EC2_PRINCIPAL)),
        ],
    );
    emulator.iam_ok(
        case,
        "AttachRolePolicy",
        &[p("RoleName", role), p("PolicyArn", SSM_CORE_POLICY)],
    );
    emulator.iam_ok(case, "CreateInstanceProfile", &[p("InstanceProfileName", profile)]);
    emulator.iam_ok(
        case,
        "AddRoleToInstanceProfile",
        &[p("InstanceProfileName", profile), p("RoleName", role)],
    );
}

/// The image id the public Systems Manager parameter resolves to — the call a client resolves
/// its AMI with.
fn image_id(emulator: &Emulator) -> String {
    let answered = emulator.ssm_ok(
        "the image",
        "GetParameter",
        &json!({ "Name": AL2023_PARAMETER }),
    );
    answered["Parameter"]["Value"]
        .as_str()
        .unwrap_or_else(|| panic!("GetParameter must report an image id, got {answered}"))
        .to_string()
}

/// The network every group places into: one VPC and one subnet of it in [`ZONE`].
fn seed_subnet(emulator: &Emulator) -> String {
    let case = "the network";
    let vpc = element(
        &emulator.ec2_ok(case, "CreateVpc", &[p("CidrBlock", "10.70.0.0/16")]),
        "vpcId",
    );
    element(
        &emulator.ec2_ok(
            case,
            "CreateSubnet",
            &[
                p("VpcId", vpc),
                p("CidrBlock", "10.70.1.0/24"),
                p("AvailabilityZone", ZONE),
            ],
        ),
        "subnetId",
    )
}

/// How the group's launch source comes to resolve the version the refresh moves its guests
/// onto.
#[derive(Debug, Clone, Copy)]
enum Moved {
    /// The group names version 1 and is updated to the number the new version was minted as.
    ToTheNumber,
    /// The group floats on `$Latest`, so publishing the version is what moves it
    /// (`LaunchTemplateSpecification$Version`: "The version number, `$Latest`, or
    /// `$Default`").
    OnLatest,
    /// The group floats on `$Default`, and the template's default version is moved to the new
    /// one.
    OnDefault,
}

/// The FORM a generation's user data takes, which is what writes its marker in the guest.
#[derive(Debug, Clone, Copy)]
enum Program {
    /// A user-data script, which cloud-init runs once per instance.
    Script,
    /// A `#cloud-config` document whose `write_files` module names the file.
    CloudConfig,
}

/// One row: how many guests the group holds, how its launch source reaches the new version,
/// and the form the user data of both its generations takes.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    /// The group's desired capacity, which is how many guests the refresh has to replace.
    capacity: usize,
    moved: Moved,
    program: Program,
}

const ROWS: &[Row] = &[
    Row {
        case: "two guests, the group updated to the numbered version, a user-data script",
        capacity: 2,
        moved: Moved::ToTheNumber,
        program: Program::Script,
    },
    Row {
        case: "one guest floating on $Latest, a cloud-config document",
        capacity: 1,
        moved: Moved::OnLatest,
        program: Program::CloudConfig,
    },
    Row {
        case: "two guests floating on $Default, the default version moved, a user-data script",
        capacity: 2,
        moved: Moved::OnDefault,
        program: Program::Script,
    },
];

/// The marker generation `generation` of row `index` writes: the row's own, so no guest can
/// report another row's marker or its own earlier generation's and pass.
fn marker(index: usize, generation: u32) -> String {
    format!("lt{index}-generation-{generation}")
}

/// The user data one generation carries: the program that leaves `marker` at
/// [`GENERATION_PATH`], in the form the row names.
fn user_data(program: Program, marker: &str) -> String {
    match program {
        Program::Script => format!("#!/bin/bash\necho {marker} > {GENERATION_PATH}\n"),
        Program::CloudConfig => format!(
            "#cloud-config\nwrite_files:\n  - path: {GENERATION_PATH}\n    content: |\n      \
             {marker}\n"
        ),
    }
}

/// The version word the group's launch source carries while it holds generation 1.
fn initial_version(moved: Moved) -> &'static str {
    match moved {
        Moved::ToTheNumber => "1",
        Moved::OnLatest => "$Latest",
        Moved::OnDefault => "$Default",
    }
}

/// Create the template of one row with generation 1's user data, and answer its name.
fn create_template(emulator: &Emulator, row: &Row, index: usize, image: &str) -> String {
    let name = format!("asg-refresh-generation-lt-{index}");
    emulator.ec2_ok(
        row.case,
        "CreateLaunchTemplate",
        &[
            p("LaunchTemplateName", name.clone()),
            data("ImageId", image),
            data("InstanceType", INSTANCE_TYPE),
            // "A session token is required" — what a client that wants IMDSv2 only declares,
            // which is the metadata service the probe below reads.
            data("MetadataOptions.HttpTokens", "required"),
            data("IamInstanceProfile.Name", PROFILE_NAME),
            data(
                "UserData",
                aws_smithy_types::base64::encode(user_data(row.program, &marker(index, 1))),
            ),
        ],
    );
    name
}

/// Create the group of one row at its capacity, and answer its name.
///
/// `MaxSize` leaves room for ONE surged instance, which is what a refresh preferring a
/// hundred-percent minimum healthy capacity needs to launch a replacement before it takes the
/// instance it replaces out.
fn create_group(emulator: &Emulator, row: &Row, index: usize, template: &str, subnet: &str) -> String {
    let name = format!("asg-refresh-generation-{index}");
    emulator.asg_ok(
        row.case,
        "CreateAutoScalingGroup",
        &[
            p("AutoScalingGroupName", name.clone()),
            p("LaunchTemplate.LaunchTemplateName", template),
            p("LaunchTemplate.Version", initial_version(row.moved)),
            p("MinSize", row.capacity.to_string()),
            p("MaxSize", (row.capacity + 1).to_string()),
            p("DesiredCapacity", row.capacity.to_string()),
            p("VPCZoneIdentifier", subnet),
        ],
    );
    name
}

/// The ids the group `name` reports [`IN_SERVICE`] and [`HEALTHY`] — what a client counts its
/// serving capacity by.
fn in_service_ids(emulator: &Emulator, case: &str, name: &str) -> Vec<String> {
    let described = emulator.asg_ok(
        case,
        "DescribeAutoScalingGroups",
        &[p("AutoScalingGroupNames.member.1", name)],
    );
    let held = block_of(&described, "Instances").unwrap_or_default();
    members_of(held)
        .into_iter()
        .filter(|member| {
            elements(member, "LifecycleState")
                .iter()
                .any(|state| state == IN_SERVICE)
                && elements(member, "HealthStatus")
                    .iter()
                    .any(|health| health == HEALTHY)
        })
        .filter_map(|member| elements(member, "InstanceId").into_iter().next())
        .collect()
}

/// Look again until the group holds exactly `wanted` serving instances, none of them in
/// `excluded`, and answer their ids.
fn settled_in_service_ids(
    emulator: &Emulator,
    case: &str,
    name: &str,
    wanted: usize,
    excluded: &[String],
) -> Vec<String> {
    let until = Instant::now() + DEADLINE;
    loop {
        let serving = in_service_ids(emulator, case, name);
        if serving.len() == wanted
            && !serving
                .iter()
                .any(|instance| excluded.contains(instance))
        {
            return serving;
        }
        assert!(
            Instant::now() < until,
            "case {case:?}: expected the group {name} to hold {wanted} serving instances \
             outside {excluded:?}, got {serving:?}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// The `InstanceStateName` Amazon EC2 reports for `instance_id`.
fn instance_state(emulator: &Emulator, case: &str, instance_id: &str) -> String {
    let described = emulator.ec2_ok(case, "DescribeInstances", &[p("InstanceId.1", instance_id)]);
    let state = block_of(&described, "instanceState").unwrap_or_default();
    elements(state, "name").into_iter().next().unwrap_or_default()
}

/// Look again until Amazon EC2 reports `instance_id` [`TERMINATED`].
fn until_terminated(emulator: &Emulator, case: &str, instance_id: &str) {
    let until = Instant::now() + DEADLINE;
    loop {
        let reported = instance_state(emulator, case, instance_id);
        if reported == TERMINATED {
            return;
        }
        assert!(
            Instant::now() < until,
            "case {case:?}: expected Amazon EC2 to report the replaced instance {instance_id} \
             {TERMINATED}, got {reported:?}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Look again until Systems Manager reports `instance_id` as a managed node whose agent is
/// responding — the barrier a client waits at before it sends a command.
fn until_online(emulator: &Emulator, case: &str, instance_id: &str) {
    let until = Instant::now() + DEADLINE;
    loop {
        let reported = emulator.ssm_ok(
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
            return;
        }
        assert!(
            Instant::now() < until,
            "case {case:?}: expected {instance_id} to become a managed node reporting \
             {ONLINE}, last read {reported}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// The script a client asks a guest for its own identity and its generation with: an IMDSv2
/// token, the instance id the metadata service publishes to that guest, and the marker the
/// version's user data left on it.
fn probe() -> Vec<String> {
    // The metadata service's own address, read from the service that publishes it to its guest
    // rather than written here.
    let metadata = format!(
        "http://{}",
        service_com_amazonaws_ec2_amazonec2::state::metadata_address()
    );
    vec![
        format!(
            "TOKEN=$(curl -X PUT {metadata}/latest/api/token \
             -H \"X-aws-ec2-metadata-token-ttl-seconds: 21600\")"
        ),
        format!(
            "echo iid=$(curl -s -H \"X-aws-ec2-metadata-token: $TOKEN\" \
             {metadata}/latest/meta-data/instance-id)"
        ),
        format!("echo gen=$(cat {GENERATION_PATH})"),
    ]
}

/// Run [`probe`] in the guest of `instance_id` and answer what it wrote on standard output,
/// looking again until the invocation has ended.
fn probed_in_guest(emulator: &Emulator, case: &str, instance_id: &str) -> String {
    let sent = emulator.ssm_ok(
        case,
        "SendCommand",
        &json!({
            "DocumentName": SHELL_DOCUMENT,
            "InstanceIds": [instance_id],
            "Parameters": { "commands": probe() },
        }),
    );
    let command_id = sent["Command"]["CommandId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: SendCommand must mint a command id, got {sent}"))
        .to_string();
    let until = Instant::now() + DEADLINE;
    loop {
        let reported = emulator.ssm_ok(
            case,
            "GetCommandInvocation",
            &json!({ "CommandId": command_id, "InstanceId": instance_id }),
        );
        let reached = reported["Status"].as_str().unwrap_or_default().to_string();
        if TERMINAL_INVOCATION_STATUSES.contains(&reached.as_str()) {
            assert_eq!(
                reached, INVOCATION_SUCCESS,
                "case {case:?}: expected the probe of {instance_id} to run cleanly in its \
                 guest, got {reported}"
            );
            return reported["StandardOutputContent"]
                .as_str()
                .unwrap_or_default()
                .to_string();
        }
        assert!(
            Instant::now() < until,
            "case {case:?}: expected the probe of {instance_id} to finish, last read {reported}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Assert that the guest of `instance_id` reports its OWN id and the generation `marker`.
fn guest_reports(emulator: &Emulator, case: &str, instance_id: &str, marker: &str) {
    let written = probed_in_guest(emulator, case, instance_id);
    let expected = format!("iid={instance_id}\ngen={marker}\n");
    assert_eq!(
        written, expected,
        "case {case:?}: expected the guest of {instance_id} to report {expected:?}, got \
         {written:?}"
    );
}

/// The newest refresh of one group as `DescribeInstanceRefreshes` reports it: its id, its
/// status and how far it reports itself.
#[derive(Debug)]
struct ReportedRefresh {
    instance_refresh_id: String,
    status: String,
    percentage_complete: String,
}

fn newest_refresh(emulator: &Emulator, case: &str, group: &str) -> ReportedRefresh {
    let described = emulator.asg_ok(
        case,
        "DescribeInstanceRefreshes",
        &[p("AutoScalingGroupName", group)],
    );
    ReportedRefresh {
        instance_refresh_id: element(&described, "InstanceRefreshId"),
        status: element(&described, "Status"),
        percentage_complete: element(&described, "PercentageComplete"),
    }
}

/// Look again until the group's newest refresh reports a status it has ENDED in.
fn until_terminal_refresh(emulator: &Emulator, case: &str, group: &str) -> ReportedRefresh {
    let until = Instant::now() + DEADLINE;
    loop {
        let reported = newest_refresh(emulator, case, group);
        if TERMINAL_REFRESH_STATUSES.contains(&reported.status.as_str()) {
            return reported;
        }
        assert!(
            Instant::now() < until,
            "case {case:?}: expected the refresh of {group} to reach a terminal status, last \
             read {reported:?}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// The instance profile every group's instances run as, and the role it carries.
const PROFILE_NAME: &str = "asg-refresh-generation-profile";
const ROLE_NAME: &str = "asg-refresh-generation-role";

/// What one row holds once it has been set up: the names the calls below address it by, the
/// ids it was serving before the refresh, and the refresh it started.
struct Standing {
    row: Row,
    index: usize,
    template: String,
    group: String,
    before: Vec<String>,
    started: String,
}

#[test]
fn an_instance_refresh_rolls_every_guest_onto_the_new_launch_template_version() {
    std::thread::Builder::new()
        .name("asg-refresh-generation".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let emulator = Emulator::new();
            create_profile(&emulator, PROFILE_NAME, ROLE_NAME);
            let image = image_id(&emulator);
            let subnet = seed_subnet(&emulator);

            // Every row's template and group is created FIRST, so the spans of the rows run
            // side by side rather than one after another.
            let mut standing: Vec<Standing> = Vec::new();
            for (index, row) in ROWS.iter().enumerate() {
                let template = create_template(&emulator, row, index, &image);
                let group = create_group(&emulator, row, index, &template, &subnet);
                standing.push(Standing {
                    row: *row,
                    index,
                    template,
                    group,
                    before: Vec::new(),
                    started: String::new(),
                });
            }

            // The barrier a client waits at before it rolls anything out: the group serves the
            // capacity it was created with.
            for held in &mut standing {
                held.before = settled_in_service_ids(
                    &emulator,
                    held.row.case,
                    &held.group,
                    held.row.capacity,
                    &[],
                );
            }

            // Each of those guests is on generation 1, read from inside it: this is what makes
            // the generation 2 reads below an observation rather than an assumption.
            for held in &standing {
                let first = marker(held.index, 1);
                for instance in &held.before {
                    until_online(&emulator, held.row.case, instance);
                    guest_reports(&emulator, held.row.case, instance, &first);
                }
            }

            // Generation 2: one new version per row, and the launch source moved onto it the
            // way the row names.
            for held in &standing {
                let case = held.row.case;
                let second = marker(held.index, 2);
                let created = emulator.ec2_ok(
                    case,
                    "CreateLaunchTemplateVersion",
                    &[
                        p("LaunchTemplateName", held.template.clone()),
                        p("SourceVersion", "1"),
                        data(
                            "UserData",
                            aws_smithy_types::base64::encode(user_data(
                                held.row.program,
                                &second,
                            )),
                        ),
                    ],
                );
                let number = element(&created, "versionNumber");
                assert_eq!(
                    number, "2",
                    "case {case:?}: the second version of a template is numbered 2, got \
                     {created}"
                );
                match held.row.moved {
                    Moved::ToTheNumber => {
                        emulator.asg_ok(
                            case,
                            "UpdateAutoScalingGroup",
                            &[
                                p("AutoScalingGroupName", held.group.clone()),
                                p("LaunchTemplate.LaunchTemplateName", held.template.clone()),
                                p("LaunchTemplate.Version", number),
                            ],
                        );
                    }
                    // A group floating on `$Latest` resolves the version that was just
                    // published, so nothing else moves it.
                    Moved::OnLatest => {}
                    Moved::OnDefault => {
                        emulator.ec2_ok(
                            case,
                            "ModifyLaunchTemplate",
                            &[
                                p("LaunchTemplateName", held.template.clone()),
                                p("SetDefaultVersion", number),
                            ],
                        );
                    }
                }
            }

            // The rollout itself: a refresh that keeps the whole of the group's capacity
            // healthy, surging each replacement before it takes the instance it replaces out,
            // and waiting nothing after one comes into service.
            for held in &mut standing {
                let started = emulator.asg_ok(
                    held.row.case,
                    "StartInstanceRefresh",
                    &[
                        p("AutoScalingGroupName", held.group.clone()),
                        p("Preferences.MinHealthyPercentage", "100"),
                        p("Preferences.MaxHealthyPercentage", "200"),
                        p("Preferences.InstanceWarmup", "0"),
                    ],
                );
                let id = element(&started, "InstanceRefreshId");
                assert!(
                    !id.is_empty(),
                    "case {:?}: a started refresh carries its id, got {started}",
                    held.row.case
                );
                held.started = id;
            }

            for held in &standing {
                let case = held.row.case;
                let reached = until_terminal_refresh(&emulator, case, &held.group);
                assert_eq!(
                    reached.status, SUCCESSFUL,
                    "case {case:?}: expected the refresh to end {SUCCESSFUL}, got {reached:?}"
                );
                assert_eq!(
                    reached.instance_refresh_id, held.started,
                    "case {case:?}: expected the group's newest refresh to be the one that was \
                     started, got {reached:?}"
                );
                assert_eq!(
                    reached.percentage_complete, "100",
                    "case {case:?}: a refresh that replaced every instance reports itself \
                     complete, got {reached:?}"
                );

                // The group holds its capacity again, in instances none of which it held
                // before — and every instance it did hold is terminated.
                let after = settled_in_service_ids(
                    &emulator,
                    case,
                    &held.group,
                    held.row.capacity,
                    &held.before,
                );
                for replaced in &held.before {
                    assert!(
                        !after.contains(replaced),
                        "case {case:?}: expected the replaced instance {replaced} to be gone \
                         from the group, got {after:?}"
                    );
                    until_terminated(&emulator, case, replaced);
                }

                // And each replacement is a real guest of the NEW version: it registers as a
                // managed node and reports its own id and generation 2's marker from inside.
                let second = marker(held.index, 2);
                for instance in &after {
                    until_online(&emulator, case, instance);
                    guest_reports(&emulator, case, instance, &second);
                }
            }
        })
        .expect("a sized thread")
        .join()
        .expect("the suite thread");
}
