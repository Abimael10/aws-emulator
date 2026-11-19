// SPDX-License-Identifier: CC0-1.0

//! Session Manager access to the instances an AUTO SCALING GROUP brought into service, on
//! the whole assembled emulator a client talks to.
//!
//! RULE under test: an instance an Auto Scaling group launches from a launch template becomes
//! a Systems Manager managed node exactly when the IAM instance profile that launch source
//! names carries a role Amazon EC2 may act as whose identity-based policies let the agent
//! perform the call it registers with — the same condition that decides it for an instance
//! launched any other way, because the registration is derived from the instance, its
//! instance-profile association and that profile's role rather than from which call made the
//! launch. A span after the launch, `DescribeInstanceInformation` reports such an instance
//! with `PingStatus` `Online` (`smithy_query(query="shape", shape="PingStatus")`:
//! `Online | ConnectionLost | Inactive`), and `StartSession(Target=<that id>)` is served with
//! a `SessionId`, a `TokenValue` and a `StreamUrl` back to this emulator, which
//! `TerminateSession` then closes by echoing that `SessionId`
//! (`smithy_query(query="operation", operation="StartSession,TerminateSession")`). A target
//! this endpoint holds no responding node for — an instance of a sibling group whose role
//! withholds that call, one launched with no profile at all, an id nothing was launched as —
//! is absent from every page of the report, and a start against it is refused
//! `TargetNotConnected` (HTTP 400) with the fixed text ` is not connected.` after the target
//! the request named.
//!
//! The rows differ along what the rule generalises over: how the launch template spells the
//! profile reference (`Name`, `Arn`, none at all), what the profile's role says about the
//! registration call (the AWS-managed policy a client attaches for it, an inline grant of
//! exactly that call, an inline wildcard over the service, no policy at all), HOW the group
//! reaches its capacity (declared at create, raised afterwards by `UpdateAutoScalingGroup`,
//! raised by `SetDesiredCapacity`), and WHERE the instance is placed (the group's
//! `VPCZoneIdentifier`, or a network interface the launch template declares in the subnet
//! with a public address, as a client that wants the agent to reach the service writes it).
//!
//! This suite drives the ASSEMBLED router — every service mounted on one engine, as
//! `core/router/src/lib.rs` assembles it — because the chain crosses four of them: a client
//! resolves its image id through Systems Manager's own public parameter, builds the roles and
//! instance profiles through IAM, builds the network and the launch templates through Amazon
//! EC2, scales through Amazon EC2 Auto Scaling and then asks Systems Manager for the node and
//! the session. What the owning crates assert beside it is narrower: the registration's own
//! table of grants is
//! `services/service-com-amazonaws-ssm-amazonssm/tests/all/ec2_managed_node_registration.rs`,
//! and the instance-profile association a group's launch writes is
//! `services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/tests/all/launch_source_instance_profile.rs`.
//! What this suite adds is the SESSION half over the group's launch path, read where a client
//! reads it. Assembling the whole router costs stack (see `boot_serve.rs`), so it is built —
//! and the requests driven — on a sized thread.
//!
//! Nothing here waits a fixed time: every span crossed (the group's own capacity change,
//! Amazon EC2's move to `running`, the registration) is a stored due instant, so a case LOOKS
//! AGAIN until the condition holds or [`DEADLINE`] passes, and no assertion is made about
//! elapsed time. The rows that must NOT register are created BEFORE the rows that must, so
//! that by the time the last registration has been observed every earlier instance's own span
//! has passed too — which is what makes "it is not connected" an answer rather than a race.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The form encoder and the XML readers an awsQuery/ec2Query suite needs live ONCE in the
// test crate, in `query_wire`.
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
/// through, which is the image every template here launches from.
const AL2023_PARAMETER: &str =
    "/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64";

/// The instance type every launch template here names.
const INSTANCE_TYPE: &str = "t3.micro";

/// The AWS-managed policy a client attaches to an instance profile's role so the agent on the
/// instance may register it. Its document is AWS's own, so this emulator holds none for it.
const SSM_CORE_POLICY: &str = "arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore";

/// The inline document granting exactly the call the agent registers a node with.
const GRANTS_REGISTRATION: &str = "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\
    \"Allow\",\"Action\":\"ssm:UpdateInstanceInformation\",\"Resource\":\"*\"}]}";

/// The inline document granting every action of that service.
const GRANTS_EVERY_ACTION: &str = "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\
    \"Allow\",\"Action\":\"ssm:*\",\"Resource\":\"*\"}]}";

/// The service principal a role has to admit for Amazon EC2 to act as it on an instance's
/// behalf, so the agent on that instance has credentials at all.
const EC2_PRINCIPAL: &str = "ec2.amazonaws.com";

/// The `PingStatus` of a node whose agent is responding.
const ONLINE: &str = "Online";

/// The `ResourceType` of a node that is an Amazon EC2 instance.
const RESOURCE_TYPE_EC2: &str = "EC2Instance";

/// The lifecycle state of a group's instance a client waits for before it opens a session
/// (`AutoScalingInstanceDetails$LifecycleState`), and the health that instance reports.
const IN_SERVICE: &str = "InService";
const HEALTHY: &str = "Healthy";

/// The error code a start against a target no responding node answers for is refused with.
const TARGET_NOT_CONNECTED: &str = "TargetNotConnected";

/// How long a case is given for a condition another component's clock owes.
///
/// Generous on purpose: each span is a stored due instant and nothing sleeps for one, so a
/// case looks again until the condition holds. All this bound is for is that a move that never
/// comes fails instead of hanging; nothing asserts on elapsed time.
const DEADLINE: Duration = Duration::from_secs(120);

/// How long one look-again cycle pauses before asking again. Not a bound on anything.
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
    /// The same body parsed, meaningful for a JSON wire alone, with the error shape the
    /// framework also sets as a header folded in so one accessor reads either.
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
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let text = String::from_utf8_lossy(response.body()).to_string();
    let mut body = serde_json::from_str(&text).unwrap_or(Value::Null);
    if let (Some(error_type), Value::Object(map)) = (error_type, &mut body) {
        map.entry("__type")
            .or_insert_with(|| Value::String(error_type));
    }
    Answer { status, text, body }
}

/// A form body carrying `action`, `version` and the parameters as written.
fn form_body(action: &str, version: &str, parameters: &[(String, String)]) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in parameters {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    body
}

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// One `LaunchTemplateData.<member>` parameter of a launch-template create.
fn data(member: &str, value: impl Into<String>) -> (String, String) {
    p(&format!("LaunchTemplateData.{member}"), value)
}

/// One ec2Query action of Amazon EC2 that must be served, as its XML body.
fn ec2_ok(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    action: &str,
    parameters: &[(String, String)],
) -> String {
    let answer = send(
        router,
        runtime,
        "ec2",
        "application/x-www-form-urlencoded",
        None,
        form_body(action, EC2_API_VERSION, parameters),
    );
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
    parameters: &[(String, String)],
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

/// One awsQuery action of Amazon EC2 Auto Scaling that must be served, as its XML body.
fn asg_ok(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    action: &str,
    parameters: &[(String, String)],
) -> String {
    let answer = send(
        router,
        runtime,
        "autoscaling",
        "application/x-www-form-urlencoded",
        None,
        form_body(action, ASG_API_VERSION, parameters),
    );
    assert_eq!(
        answer.status, 200,
        "case {case:?}: Auto Scaling {action} must be served, got {answer:?}"
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

/// The wire error code of a refusal (`__type`, without its namespace).
fn error_code(body: &Value) -> String {
    let rendered = body["__type"].as_str().unwrap_or("<no __type>");
    rendered
        .split('#')
        .next_back()
        .unwrap_or(rendered)
        .to_string()
}

/// The message a refusal carries, read under the member the model declares for it
/// (`smithy_query(query="shape", shape="TargetNotConnected")`: one member, `Message`), so the
/// wire shape of the refusal is part of what the assertion checks.
fn error_message(body: &Value) -> String {
    body["Message"].as_str().unwrap_or_default().to_string()
}

/// A trust policy admitting `principal` and nothing else.
fn trust_policy(principal: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"Service\":\"{principal}\"}},\"Action\":\"sts:AssumeRole\"}}]}}"
    )
}

/// The network the groups below launch into: one VPC with an internet gateway and a route to
/// it, one subnet of it, and a security group with no ingress — the shape a client builds
/// before it expects the agent on an instance to reach Systems Manager.
#[derive(Debug)]
struct Network {
    subnet: String,
    security_group: String,
}

fn seed_network(router: &MultiServiceRouter, runtime: &tokio::runtime::Runtime) -> Network {
    let case = "the network";
    let vpc = element(
        &ec2_ok(
            router,
            runtime,
            case,
            "CreateVpc",
            &[p("CidrBlock", "10.40.0.0/16")],
        ),
        "vpcId",
    );
    let gateway = element(
        &ec2_ok(router, runtime, case, "CreateInternetGateway", &[]),
        "internetGatewayId",
    );
    ec2_ok(
        router,
        runtime,
        case,
        "AttachInternetGateway",
        &[p("InternetGatewayId", gateway.clone()), p("VpcId", vpc.clone())],
    );
    let subnet = element(
        &ec2_ok(
            router,
            runtime,
            case,
            "CreateSubnet",
            &[
                p("VpcId", vpc.clone()),
                p("CidrBlock", "10.40.1.0/24"),
                p("AvailabilityZone", ZONE),
            ],
        ),
        "subnetId",
    );
    let route_table = element(
        &ec2_ok(
            router,
            runtime,
            case,
            "CreateRouteTable",
            &[p("VpcId", vpc.clone())],
        ),
        "routeTableId",
    );
    ec2_ok(
        router,
        runtime,
        case,
        "CreateRoute",
        &[
            p("RouteTableId", route_table.clone()),
            p("DestinationCidrBlock", "0.0.0.0/0"),
            p("GatewayId", gateway),
        ],
    );
    ec2_ok(
        router,
        runtime,
        case,
        "AssociateRouteTable",
        &[
            p("RouteTableId", route_table),
            p("SubnetId", subnet.clone()),
        ],
    );
    let security_group = element(
        &ec2_ok(
            router,
            runtime,
            case,
            "CreateSecurityGroup",
            &[
                p("GroupName", "asg-session-nodes"),
                p("GroupDescription", "no ingress, as a managed node needs none"),
                p("VpcId", vpc),
            ],
        ),
        "groupId",
    );
    Network {
        subnet,
        security_group,
    }
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

/// How the launch template spells the instance profile its instances run as.
#[derive(Debug, Clone, Copy)]
enum Reference {
    /// `IamInstanceProfile.Name`, the name IAM holds the profile under.
    ByName,
    /// `IamInstanceProfile.Arn`, the ARN IAM minted for it.
    ByArn,
    /// The template carries no `IamInstanceProfile` at all.
    Absent,
}

/// How the group reaches the one instance of capacity it is to hold.
#[derive(Debug, Clone, Copy)]
enum Capacity {
    /// Declared on the create itself.
    AtCreate,
    /// Created empty and raised by `UpdateAutoScalingGroup`.
    RaisedByUpdate,
    /// Created empty, its bounds raised, and the capacity set by `SetDesiredCapacity`.
    RaisedByItsOwnCall,
}

/// Where the instance's placement is declared.
#[derive(Debug, Clone, Copy)]
enum Placement {
    /// The group's own `VPCZoneIdentifier`, naming the subnet.
    GroupSubnet,
    /// A network interface the launch template declares in the subnet with a public address,
    /// the group placed by Availability Zone.
    TemplateInterface,
}

/// The instance profile IAM holds for one row: the name it is addressed by, the ARN IAM
/// minted for it, and the NAME of the one role it carries — deliberately not the profile's
/// own name, so a node reporting the profile's name as its `IamRole` is a failure rather than
/// a coincidence.
#[derive(Debug, Clone)]
struct HeldProfile {
    name: String,
    arn: String,
    role: String,
}

/// Build one instance profile the way a client builds one — `CreateRole`, the grant,
/// `CreateInstanceProfile`, `AddRoleToInstanceProfile` — and answer the ARN IAM minted for it.
fn create_profile(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    name: &str,
    grant: Grant,
) -> HeldProfile {
    let role = format!("{name}-role");
    iam_ok(
        router,
        runtime,
        case,
        "CreateRole",
        &[
            p("RoleName", role.clone()),
            p("AssumeRolePolicyDocument", trust_policy(EC2_PRINCIPAL)),
        ],
    );
    match grant {
        Grant::AwsManaged => {
            iam_ok(
                router,
                runtime,
                case,
                "AttachRolePolicy",
                &[p("RoleName", role.clone()), p("PolicyArn", SSM_CORE_POLICY)],
            );
        }
        Grant::Inline(document) => {
            iam_ok(
                router,
                runtime,
                case,
                "PutRolePolicy",
                &[
                    p("RoleName", role.clone()),
                    p("PolicyName", "agent-registration"),
                    p("PolicyDocument", document),
                ],
            );
        }
        Grant::Nothing => {}
    }
    let created = iam_ok(
        router,
        runtime,
        case,
        "CreateInstanceProfile",
        &[p("InstanceProfileName", name)],
    );
    let arn = element(&created, "Arn");
    iam_ok(
        router,
        runtime,
        case,
        "AddRoleToInstanceProfile",
        &[p("InstanceProfileName", name), p("RoleName", role.clone())],
    );
    HeldProfile {
        name: name.to_string(),
        arn,
        role,
    }
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

/// Create one launch template: the image, the instance type, the metadata options a client
/// that requires IMDSv2 sets, the profile reference under test and the placement under test.
fn create_launch_template(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    row: &Row,
    name: &str,
    image: &str,
    profile: Option<&HeldProfile>,
    network: &Network,
) {
    let case = row.case;
    let mut parameters = vec![
        p("LaunchTemplateName", name),
        data("ImageId", image),
        data("InstanceType", INSTANCE_TYPE),
        // "A session token is required" — what a client that wants IMDSv2 only declares.
        data("MetadataOptions.HttpTokens", "required"),
    ];
    match (row.reference, profile) {
        (Reference::ByName, Some(held)) => {
            parameters.push(data("IamInstanceProfile.Name", held.name.clone()));
        }
        (Reference::ByArn, Some(held)) => {
            parameters.push(data("IamInstanceProfile.Arn", held.arn.clone()));
        }
        (Reference::Absent, None) => {}
        // A row that names a reference with no profile built for it would test nothing, so
        // it fails here rather than quietly launching with no profile at all.
        (reference, held) => panic!(
            "case {case:?}: a row naming {reference:?} needs a profile, got {held:?}"
        ),
    }
    match row.placement {
        Placement::GroupSubnet => {
            parameters.push(data("SecurityGroupId.1", network.security_group.clone()));
        }
        Placement::TemplateInterface => {
            parameters.push(data("NetworkInterface.1.DeviceIndex", "0"));
            parameters.push(data("NetworkInterface.1.SubnetId", network.subnet.clone()));
            parameters.push(data(
                "NetworkInterface.1.AssociatePublicIpAddress",
                "true",
            ));
            parameters.push(data(
                "NetworkInterface.1.SecurityGroupId.1",
                network.security_group.clone(),
            ));
        }
    }
    ec2_ok(router, runtime, case, "CreateLaunchTemplate", &parameters);
}

/// Create one group launching from `template` and bring it to one instance of capacity the way
/// `capacity` says.
fn create_group(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    row: &Row,
    name: &str,
    template: &str,
    network: &Network,
) {
    let case = row.case;
    let wanted = matches!(row.capacity, Capacity::AtCreate);
    let mut parameters = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", template),
        p("MinSize", if wanted { "1" } else { "0" }),
        p("MaxSize", if wanted { "1" } else { "0" }),
        p("DesiredCapacity", if wanted { "1" } else { "0" }),
    ];
    match row.placement {
        // A template that declares its own interface in the subnet is placed by zone, the way
        // a client that put the subnet on the interface places it.
        Placement::TemplateInterface => {
            parameters.push(p("AvailabilityZones.member.1", ZONE));
        }
        Placement::GroupSubnet => {
            parameters.push(p("VPCZoneIdentifier", network.subnet.clone()));
        }
    }
    asg_ok(
        router,
        runtime,
        case,
        "CreateAutoScalingGroup",
        &parameters,
    );
    match row.capacity {
        Capacity::AtCreate => {}
        Capacity::RaisedByUpdate => {
            asg_ok(
                router,
                runtime,
                case,
                "UpdateAutoScalingGroup",
                &[
                    p("AutoScalingGroupName", name),
                    p("MinSize", "1"),
                    p("MaxSize", "1"),
                    p("DesiredCapacity", "1"),
                ],
            );
        }
        Capacity::RaisedByItsOwnCall => {
            asg_ok(
                router,
                runtime,
                case,
                "UpdateAutoScalingGroup",
                &[
                    p("AutoScalingGroupName", name),
                    p("MinSize", "0"),
                    p("MaxSize", "1"),
                ],
            );
            asg_ok(
                router,
                runtime,
                case,
                "SetDesiredCapacity",
                &[
                    p("AutoScalingGroupName", name),
                    p("DesiredCapacity", "1"),
                ],
            );
        }
    }
}

/// The one instance the group `name` holds once it reports it [`IN_SERVICE`] and [`HEALTHY`],
/// looked for again until it does — the barrier a client waits at before it opens a session.
fn in_service_instance(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    name: &str,
) -> String {
    let until = Instant::now() + DEADLINE;
    loop {
        let described = asg_ok(
            router,
            runtime,
            case,
            "DescribeAutoScalingGroups",
            &[p("AutoScalingGroupNames.member.1", name)],
        );
        let held = block_of(&described, "Instances").unwrap_or_default();
        let served: Vec<String> = members_of(held)
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
            .collect();
        if let [instance_id] = served.as_slice() {
            return instance_id.clone();
        }
        assert!(
            Instant::now() < until,
            "case {case:?}: expected the group {name} to hold one {IN_SERVICE} {HEALTHY} \
             instance, got {served:?} in {described}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// The instance-profile ARN Amazon EC2 reports `instance_id` runs as, or `None` when it
/// reports none.
fn instance_profile_arn(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    instance_id: &str,
) -> Option<String> {
    let described = ec2_ok(
        router,
        runtime,
        case,
        "DescribeInstances",
        &[p("InstanceId.1", instance_id)],
    );
    let reported = block_of(&described, "iamInstanceProfile")?;
    elements(reported, "arn").into_iter().next()
}

/// The `InstanceInformation` Systems Manager reports for `instance_id`, or `None` when it
/// reports none — read through the filter a client narrows the report with, following
/// `NextToken` so "absent" means absent from EVERY page.
fn node(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    instance_id: &str,
) -> Option<Value> {
    let mut request = json!({ "Filters": [{ "Key": "InstanceIds", "Values": [instance_id] }] });
    loop {
        let reported = ssm_ok(
            router,
            runtime,
            case,
            "DescribeInstanceInformation",
            &request,
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
        if let Some(found) = listed.first() {
            return Some(found.clone());
        }
        match reported["NextToken"].as_str() {
            Some(token) if !token.is_empty() => {
                request["NextToken"] = Value::String(token.to_string());
            }
            _ => return None,
        }
    }
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

/// Whether the sessions `state` holds at this endpoint carry one under `session_id`, and what
/// target it names.
fn session_target(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    state: &str,
    session_id: &str,
) -> Option<String> {
    let reported = ssm_ok(
        router,
        runtime,
        case,
        "DescribeSessions",
        &json!({ "State": state }),
    );
    reported["Sessions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|session| session["SessionId"].as_str() == Some(session_id))
        .map(|session| {
            session["Target"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
}

/// One row: a group launching from a template that names a profile a given way, a role granted
/// (or withheld) the registration call, the way the capacity is reached and where the instance
/// is placed.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    reference: Reference,
    grant: Grant,
    capacity: Capacity,
    placement: Placement,
    /// Whether the instance must become a managed node a session opens to.
    connects: bool,
}

/// The table, with every row that must NOT register before every row that must: the spans are
/// per instance, so observing the last registration proves every earlier instance's own span
/// has passed.
const ROWS: &[Row] = &[
    Row {
        case: "a profile whose role carries no policy, named by name",
        reference: Reference::ByName,
        grant: Grant::Nothing,
        capacity: Capacity::AtCreate,
        placement: Placement::GroupSubnet,
        connects: false,
    },
    Row {
        case: "a launch template naming no instance profile at all, raised by an update",
        reference: Reference::Absent,
        grant: Grant::Nothing,
        capacity: Capacity::RaisedByUpdate,
        placement: Placement::TemplateInterface,
        connects: false,
    },
    Row {
        case: "the AWS-managed core policy, named by name, on an interface the template \
               declares in the subnet",
        reference: Reference::ByName,
        grant: Grant::AwsManaged,
        capacity: Capacity::AtCreate,
        placement: Placement::TemplateInterface,
        connects: true,
    },
    Row {
        case: "an inline grant of the registration call, named by ARN, capacity raised by an \
               update",
        reference: Reference::ByArn,
        grant: Grant::Inline(GRANTS_REGISTRATION),
        capacity: Capacity::RaisedByUpdate,
        placement: Placement::GroupSubnet,
        connects: true,
    },
    Row {
        case: "an inline wildcard over the service, capacity set by SetDesiredCapacity",
        reference: Reference::ByName,
        grant: Grant::Inline(GRANTS_EVERY_ACTION),
        capacity: Capacity::RaisedByItsOwnCall,
        placement: Placement::GroupSubnet,
        connects: true,
    },
];

/// Targets no instance of this account was ever launched as, which a start is refused for the
/// same way a sibling instance that never registered is: an instance id nothing holds, a
/// hybrid-activation node id nothing registered, and a value that is no node id at all.
const UNLAUNCHED_TARGETS: &[(&str, &str)] = &[
    ("an instance id nothing was launched as", "i-00000000000000000"),
    (
        "a hybrid-activation node id nothing registered",
        "mi-0123456789abcdef0",
    ),
    ("a value that is no node id at all", "not-a-managed-node"),
];

#[test]
fn a_groups_instance_takes_a_session_exactly_when_its_profile_lets_the_agent_register() {
    std::thread::Builder::new()
        .name("asg-session-nodes".to_string())
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

            let network = seed_network(&router, &runtime);
            let image = image_id(&router, &runtime, AL2023_PARAMETER);

            // Every group is created and scaled FIRST, so the spans of the rows run side by
            // side rather than one after another.
            let mut launched: Vec<(Row, String, Option<HeldProfile>)> = Vec::new();
            for (index, row) in ROWS.iter().enumerate() {
                let profile = match row.reference {
                    Reference::Absent => None,
                    _ => Some(create_profile(
                        &router,
                        &runtime,
                        row.case,
                        &format!("asg-session-profile-{index}"),
                        row.grant,
                    )),
                };
                let template = format!("asg-session-lt-{index}");
                create_launch_template(
                    &router,
                    &runtime,
                    row,
                    &template,
                    &image,
                    profile.as_ref(),
                    &network,
                );
                let group = format!("asg-session-group-{index}");
                create_group(&router, &runtime, row, &group, &template, &network);
                launched.push((*row, group, profile));
            }

            // The barrier a client waits at: each group holds one instance it reports in
            // service, and that instance runs as the profile its launch source named.
            let instances: Vec<String> = launched
                .iter()
                .map(|(row, group, profile)| {
                    let instance = in_service_instance(&router, &runtime, row.case, group);
                    let reported =
                        instance_profile_arn(&router, &runtime, row.case, &instance);
                    let expected = profile.as_ref().map(|held| held.arn.clone());
                    assert_eq!(
                        reported, expected,
                        "case {:?}: expected Amazon EC2 to report the instance profile \
                         {expected:?} over {instance}, got {reported:?}",
                        row.case
                    );
                    instance
                })
                .collect();

            // The rows whose role lets the agent register: a node reported Online, and a
            // session that opens to it and closes again.
            for ((row, _, profile), instance) in launched.iter().zip(&instances) {
                if !row.connects {
                    continue;
                }
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
                let role = profile.as_ref().map(|held| held.role.clone());
                assert_eq!(
                    reported["IamRole"].as_str().map(str::to_string),
                    role,
                    "case {:?}: expected the node to report the role {role:?} its profile \
                     carries, got {reported}",
                    row.case
                );

                let started = ssm_ok(
                    &router,
                    &runtime,
                    row.case,
                    "StartSession",
                    &json!({ "Target": instance }),
                );
                let session_id = started["SessionId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                assert!(
                    !session_id.is_empty(),
                    "case {:?}: expected a SessionId for {instance}, got {started}",
                    row.case
                );
                let token = started["TokenValue"].as_str().unwrap_or_default();
                assert!(
                    !token.is_empty(),
                    "case {:?}: expected a TokenValue for {instance}, got {started}",
                    row.case
                );
                // "A URL back to SSM Agent on the managed node … Format:
                // wss://<host>/v1/data-channel/session-id?stream=(input|output)", on the
                // authority THIS emulator is served at rather than a Systems Manager host:
                // the URL is taken apart so each half is asserted rather than matched as a
                // substring of the whole.
                let stream_url = started["StreamUrl"].as_str().unwrap_or_default();
                let (authority, path_and_query) = stream_url
                    .strip_prefix("wss://")
                    .and_then(|rest| rest.split_once('/'))
                    .unwrap_or_else(|| {
                        panic!(
                            "case {:?}: expected a wss:// StreamUrl with a path, got {started}",
                            row.case
                        )
                    });
                assert!(
                    !authority.is_empty() && !authority.ends_with("amazonaws.com"),
                    "case {:?}: expected the StreamUrl to lead back to this emulator rather \
                     than to an AWS endpoint, got authority {authority:?}",
                    row.case
                );
                let (path, stream) = path_and_query.split_once('?').unwrap_or_default();
                assert_eq!(
                    path,
                    format!("v1/data-channel/{session_id}"),
                    "case {:?}: expected the modelled data-channel path of {session_id}, got \
                     {stream_url}",
                    row.case
                );
                assert!(
                    stream == "stream=input" || stream == "stream=output",
                    "case {:?}: expected the modelled stream query of the data channel, got \
                     {stream:?} in {stream_url}",
                    row.case
                );

                // The session is the account's own: it is reported Active against the
                // instance it was opened to, which is what makes the start more than an
                // answer.
                let held = session_target(&router, &runtime, row.case, "Active", &session_id);
                assert_eq!(
                    held.as_deref(),
                    Some(instance.as_str()),
                    "case {:?}: expected the Active session {session_id} to name \
                     {instance}, got {held:?}",
                    row.case
                );

                let ended = ssm_ok(
                    &router,
                    &runtime,
                    row.case,
                    "TerminateSession",
                    &json!({ "SessionId": session_id }),
                );
                assert_eq!(
                    ended["SessionId"].as_str(),
                    Some(session_id.as_str()),
                    "case {:?}: expected TerminateSession to echo {session_id}, got {ended}",
                    row.case
                );
                let closed = session_target(&router, &runtime, row.case, "History", &session_id);
                assert_eq!(
                    closed.as_deref(),
                    Some(instance.as_str()),
                    "case {:?}: expected the ended session {session_id} under History \
                     naming {instance}, got {closed:?}",
                    row.case
                );
                let still_active =
                    session_target(&router, &runtime, row.case, "Active", &session_id);
                assert_eq!(
                    still_active, None,
                    "case {:?}: expected the ended session {session_id} to be Active no \
                     longer, got {still_active:?}",
                    row.case
                );
            }

            // And the rows whose role withholds that call, or that name no profile at all:
            // absent from the report while the group goes on reporting the very same
            // instance in service, and refused with the one refusal this class carries.
            // Each of these was created before the rows above, so its own span has passed.
            for ((row, group, _), instance) in launched.iter().zip(&instances) {
                if row.connects {
                    continue;
                }
                let reported = node(&router, &runtime, row.case, instance);
                assert_eq!(
                    reported, None,
                    "case {:?}: expected no managed node for {instance}, got {reported:?}",
                    row.case
                );
                let served = in_service_instance(&router, &runtime, row.case, group);
                assert_eq!(
                    served, *instance,
                    "case {:?}: expected the group {group} to go on reporting {instance} \
                     {IN_SERVICE}, got {served}",
                    row.case
                );
                refused(&router, &runtime, row.case, instance);
            }

            // The same refusal, over targets nothing of this account was launched as: the
            // message's only variable part is the target the request named.
            for (case, target) in UNLAUNCHED_TARGETS {
                refused(&router, &runtime, case, target);
            }
        })
        .expect("a sized thread")
        .join()
        .expect("the suite thread");
}

/// A start against `target` is refused `TargetNotConnected` with HTTP 400 and the one message
/// this class carries: the target the request named, then the fixed text.
fn refused(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    target: &str,
) {
    let answer = ssm(router, runtime, "StartSession", &json!({ "Target": target }));
    assert_eq!(
        answer.status, 400,
        "case {case:?}: expected a start against {target} to be refused 400, got {answer:?}"
    );
    let code = error_code(&answer.body);
    assert_eq!(
        code, TARGET_NOT_CONNECTED,
        "case {case:?}: expected {TARGET_NOT_CONNECTED}, got {code} in {:?}",
        answer.body
    );
    let message = error_message(&answer.body);
    let expected = format!("{target} is not connected.");
    assert_eq!(
        message, expected,
        "case {case:?}: expected the refusal to read {expected:?}, got {message:?}"
    );
}
