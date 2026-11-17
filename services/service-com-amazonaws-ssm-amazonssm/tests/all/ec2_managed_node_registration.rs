// SPDX-License-Identifier: CC0-1.0

//! Managed-node registration end to end: an Amazon EC2 instance whose IAM instance profile
//! lets the SSM Agent register becomes a managed node of Systems Manager, and its own
//! information is the inventory a query reads back.
//!
//! RULE under test: an instance is a managed node exactly when the three components that
//! own the evidence say so — Amazon EC2 holds the instance `running`, it holds an
//! association between that instance and an IAM instance profile, and IAM holds that
//! profile with a role Amazon EC2 may act as whose policies let the agent perform the call
//! it registers with. Registration falls due a span after the launch, whether or not
//! anybody calls; from then `DescribeInstanceInformation` reports the node `Online`,
//! `GetInventory` reports its `AWS:InstanceInformation` item, and an association created
//! for it runs on it. An instance Amazon EC2 no longer holds `running` is reported by
//! neither.
//!
//! Every call goes through the wire a client uses: `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body for Systems Manager, an
//! `ec2Query` form POST for every Amazon EC2 call and an `awsQuery` form POST for every IAM
//! call — so each component's state is written and read through the service that owns it.
//! Nothing reaches into a state struct.
//!
//! The rows differ along what the rule generalises over: how the role is granted the
//! registration call (an AWS-managed policy attachment, an inline statement naming the
//! action, an inline wildcard over the service), how it is withheld (no policy at all, an
//! explicit deny beside a wildcard grant, a role another service may act as, no instance
//! profile, a profile IAM holds no record of), and the platform of the AMI the instance was
//! launched from.
//!
//! Nothing here waits a fixed time: every span is a stored due instant, so a case LOOKS
//! AGAIN until the condition holds or [`DEADLINE`] passes. The instances that must NOT
//! register are launched BEFORE the ones that must, so that by the time the last
//! registration has been observed every earlier instance's own span has passed too — which
//! is what makes "it is not reported" an answer rather than a race.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call_in_region, error_code, REGION};

/// The EC2 API version an `ec2Query` body carries beside its `Action`.
const EC2_API_VERSION: &str = "2016-11-15";

/// The IAM API version an `awsQuery` body carries beside its `Action`.
const IAM_API_VERSION: &str = "2010-05-08";

/// The public Systems Manager parameter the AL2023 image id is resolved through — the one
/// the issue's client resolves its AMI by.
pub(crate) const AL2023_PARAMETER: &str =
    "/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64";

/// The same for a Windows Server image, so a row can launch an instance whose AMI declares
/// the Windows platform.
const WINDOWS_PARAMETER: &str =
    "/aws/service/ami-windows-latest/Windows_Server-2022-English-Full-Base";

/// The AWS-managed policy a client attaches to an instance profile's role to let the agent
/// register. Its document is AWS's, not the account's, so this emulator holds none for it:
/// a role carrying it carries a grant that cannot be read, which is not evidence against
/// the registration.
pub(crate) const SSM_CORE_POLICY: &str = "arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore";

/// The inventory type every managed node has by being registered.
const INVENTORY_TYPE: &str = "AWS:InstanceInformation";

/// How long a case waits for a condition another component's clock owes.
///
/// Generous on purpose: the registration span, Amazon EC2's own state transition and an
/// association's run are each a stored due instant and nothing sleeps for any of them, so a
/// case looks again until the condition holds rather than waiting a fixed time.
pub(crate) const DEADLINE: Duration = Duration::from_secs(120);

/// How long one look-again cycle pauses before polling again.
pub(crate) const POLL_PAUSE: Duration = Duration::from_millis(50);

/// A router with Systems Manager, Amazon EC2 and IAM mounted on ONE shared engine.
///
/// All three are there because the registration is derived from all three: the instance and
/// its instance-profile association are Amazon EC2's, the profile and the role's policies
/// are IAM's, and the node is this service's.
pub(crate) fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signatures are not what these tests are about; the router still reads the SigV4
    // credential scope, which is where each call's Region comes from.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// One form-body call to `signing_name`'s endpoint in `region`, as its status and body text.
async fn form_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    region: &str,
    body: String,
) -> (u16, String) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.{region}.amazonaws.com/"))
        .header("host", format!("{signing_name}.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20240101/{region}/{signing_name}/\
                 aws4_request, SignedHeaders=host;x-amz-date, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(body))
        .expect("a well-formed form request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One Amazon EC2 action over the `ec2Query` wire its model declares.
///
/// Every value a case sends is free of `&`, `=`, `+` and `%`, so it travels in the form
/// body as written.
async fn ec2(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    parameters: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={EC2_API_VERSION}");
    for (name, value) in parameters {
        body.push_str(&format!("&{name}={value}"));
    }
    form_call(router, "ec2", region, body).await
}

/// One IAM action over the `awsQuery` wire its model declares. IAM is global; the request is
/// still signed for a Region, which is what its credential scope carries.
async fn iam(
    router: &MultiServiceRouter,
    action: &str,
    parameters: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={IAM_API_VERSION}");
    for (name, value) in parameters {
        body.push_str(&format!("&{name}={value}"));
    }
    form_call(router, "iam", REGION, body).await
}

/// One IAM action that must succeed.
pub(crate) async fn iam_ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    parameters: &[(&str, &str)],
) {
    let (status, body) = iam(router, action, parameters).await;
    assert_eq!(
        status, 200,
        "case {case:?}: setup {action} {parameters:?} must succeed, got {status} with {body}"
    );
}

/// The text of the first `<tag>` of an XML body, if it carries one.
fn element(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].to_string())
}

/// A trust policy admitting `principal` and nothing else, written compactly and with no
/// `&`, `=`, `+` or `%`, so it travels in a form body as written.
pub(crate) fn trust_policy(principal: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"Service\":\"{principal}\"}},\"Action\":\"sts:AssumeRole\"}}]}}"
    )
}

/// How one instance profile's role is granted — or withheld — the call the agent registers
/// with.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Grant {
    /// The AWS-managed policy a client attaches for this, whose document this emulator
    /// holds none of.
    AwsManaged,
    /// An inline policy document of the account's own.
    Inline(&'static str),
    /// No policy at all.
    None,
}

/// One instance profile a case launches an instance with.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Profile {
    /// The name of the profile and of the role it carries.
    pub(crate) name: &'static str,
    /// The service principal its role's trust policy admits.
    pub(crate) trusts: &'static str,
    /// What its role's identity-based policies say about the registration call.
    pub(crate) grant: Grant,
}

/// The inline document granting exactly the call the agent registers with.
pub(crate) const GRANTS_REGISTRATION: &str = "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\
    \"Allow\",\"Action\":\"ssm:UpdateInstanceInformation\",\"Resource\":\"*\"}]}";

/// The inline document granting every action of this service.
const GRANTS_EVERY_ACTION: &str = "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\
    \"Allow\",\"Action\":\"ssm:*\",\"Resource\":\"*\"}]}";

/// The inline document that grants every action of this service and DENIES the one the
/// agent registers with — IAM's order, where an explicit deny is final.
const DENIES_REGISTRATION: &str = "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\
    \"Allow\",\"Action\":\"ssm:*\",\"Resource\":\"*\"},{\"Effect\":\"Deny\",\"Action\":\
    \"ssm:UpdateInstanceInformation\",\"Resource\":\"*\"}]}";

/// The service principal an instance profile's role has to admit for Amazon EC2 to act as
/// it on an instance's behalf.
pub(crate) const EC2_PRINCIPAL: &str = "ec2.amazonaws.com";

/// Build one instance profile the way a client builds one — `CreateRole`, the grant, then
/// `CreateInstanceProfile` and `AddRoleToInstanceProfile` — and read it back through IAM's
/// own reader, so the profile an instance is launched with is one IAM really holds.
pub(crate) async fn create_profile(router: &MultiServiceRouter, profile: &Profile) {
    let case = profile.name;
    let trust = trust_policy(profile.trusts);
    iam_ok(
        router,
        case,
        "CreateRole",
        &[
            ("RoleName", profile.name),
            ("AssumeRolePolicyDocument", &trust),
        ],
    )
    .await;
    match profile.grant {
        Grant::AwsManaged => {
            iam_ok(
                router,
                case,
                "AttachRolePolicy",
                &[("RoleName", profile.name), ("PolicyArn", SSM_CORE_POLICY)],
            )
            .await;
        }
        Grant::Inline(document) => {
            iam_ok(
                router,
                case,
                "PutRolePolicy",
                &[
                    ("RoleName", profile.name),
                    ("PolicyName", "registration"),
                    ("PolicyDocument", document),
                ],
            )
            .await;
        }
        Grant::None => {}
    }
    iam_ok(
        router,
        case,
        "CreateInstanceProfile",
        &[("InstanceProfileName", profile.name)],
    )
    .await;
    iam_ok(
        router,
        case,
        "AddRoleToInstanceProfile",
        &[
            ("InstanceProfileName", profile.name),
            ("RoleName", profile.name),
        ],
    )
    .await;
    let (status, body) = iam(
        router,
        "GetInstanceProfile",
        &[("InstanceProfileName", profile.name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the profile must be readable through IAM, got {status} with {body}"
    );
    assert!(
        body.contains(&format!("<RoleName>{}</RoleName>", profile.name)),
        "case {case:?}: the profile must carry its role, got {body}"
    );
}

/// The image id the public Systems Manager parameter `name` resolves to, read through this
/// service's own `GetParameter`.
pub(crate) async fn image_id(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = call_in_region(router, REGION, "GetParameter", &json!({ "Name": name }))
        .await;
    assert_eq!(
        status, 200,
        "GetParameter {name} must resolve an image id, got {status} with {body}"
    );
    body["Parameter"]["Value"]
        .as_str()
        .unwrap_or_else(|| panic!("GetParameter {name} reports a value, got {body}"))
        .to_string()
}

/// Launch one instance in `region` through Amazon EC2's own `RunInstances`, optionally with
/// the instance profile `profile`, as the instance id that service answered.
pub(crate) async fn launch(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    image: &str,
    profile: Option<&str>,
) -> String {
    let mut parameters = vec![("ImageId", image), ("MinCount", "1"), ("MaxCount", "1")];
    if let Some(profile) = profile {
        parameters.push(("IamInstanceProfile.Name", profile));
    }
    let (status, body) = ec2(router, region, "RunInstances", &parameters).await;
    assert_eq!(
        status, 200,
        "case {case:?}: RunInstances must launch, got {status} with {body}"
    );
    element(&body, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: RunInstances reports an instance id: {body}"))
}

/// The `InstanceInformation` this service reports for `instance_id` in `region`, or `None`
/// when it reports none.
async fn node(router: &MultiServiceRouter, region: &str, instance_id: &str) -> Option<Value> {
    let (status, body) = call_in_region(
        router,
        region,
        "DescribeInstanceInformation",
        &json!({ "Filters": [{ "Key": "InstanceIds", "Values": [instance_id] }] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeInstanceInformation({instance_id}) must answer, got {status} with {body}"
    );
    body["InstanceInformationList"]
        .as_array()
        .and_then(|reported| reported.first().cloned())
}

/// Look again until this service reports `instance_id` as a managed node, or [`DEADLINE`]
/// passes.
pub(crate) async fn node_when_registered(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    instance_id: &str,
) -> Value {
    let until = Instant::now() + DEADLINE;
    loop {
        if let Some(reported) = node(router, region, instance_id).await {
            return reported;
        }
        assert!(
            Instant::now() < until,
            "case {case:?}: expected {instance_id} to become a managed node, and \
             DescribeInstanceInformation reported none"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// The `AWS:InstanceInformation` content `GetInventory` reports for `instance_id`, or `None`
/// when it reports no entity for it.
async fn inventory(router: &MultiServiceRouter, region: &str, instance_id: &str) -> Option<Value> {
    let (status, body) = call_in_region(
        router,
        region,
        "GetInventory",
        &json!({
            "Filters": [{
                "Key": format!("{INVENTORY_TYPE}.InstanceId"),
                "Values": [instance_id],
            }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "GetInventory({instance_id}) must answer, got {status} with {body}"
    );
    body["Entities"]
        .as_array()
        .and_then(|entities| entities.first().cloned())
        .map(|entity| entity["Data"][INVENTORY_TYPE]["Content"][0].clone())
}

/// One row of the registration table: an instance launched a given way, and whether it
/// becomes a managed node.
#[derive(Debug, Clone, Copy)]
struct Case {
    case: &'static str,
    /// The instance profile to launch with, or `None` to launch with none.
    profile: Option<&'static str>,
    /// Whether the AMI it is launched from declares the Windows platform.
    windows: bool,
    /// Whether the instance must become a managed node.
    registers: bool,
    /// The `PlatformType` the node must report, for a row that registers.
    platform_type: &'static str,
}

/// The profiles the table's rows are launched with.
fn profiles() -> Vec<Profile> {
    vec![
        Profile {
            name: "ssm-core",
            trusts: "ec2.amazonaws.com",
            grant: Grant::AwsManaged,
        },
        Profile {
            name: "ssm-inline",
            trusts: "ec2.amazonaws.com",
            grant: Grant::Inline(GRANTS_REGISTRATION),
        },
        Profile {
            name: "ssm-wildcard",
            trusts: "ec2.amazonaws.com",
            grant: Grant::Inline(GRANTS_EVERY_ACTION),
        },
        Profile {
            name: "no-grant",
            trusts: "ec2.amazonaws.com",
            grant: Grant::None,
        },
        Profile {
            name: "denied",
            trusts: "ec2.amazonaws.com",
            grant: Grant::Inline(DENIES_REGISTRATION),
        },
        Profile {
            name: "lambda-trust",
            trusts: "lambda.amazonaws.com",
            grant: Grant::AwsManaged,
        },
    ]
}

/// The table, with every row that must NOT register before every row that must: the spans
/// are per instance, so observing the last registration proves every earlier instance's own
/// span has passed.
fn registration_rows() -> Vec<Case> {
    vec![
        Case {
            case: "a role whose policies allow nothing",
            profile: Some("no-grant"),
            windows: false,
            registers: false,
            platform_type: "",
        },
        Case {
            case: "a role whose policy denies the registration call",
            profile: Some("denied"),
            windows: false,
            registers: false,
            platform_type: "",
        },
        Case {
            case: "a role another service, not Amazon EC2, may act as",
            profile: Some("lambda-trust"),
            windows: false,
            registers: false,
            platform_type: "",
        },
        Case {
            case: "an instance profile IAM holds no record of",
            profile: Some("never-created"),
            windows: false,
            registers: false,
            platform_type: "",
        },
        Case {
            case: "an instance running as no instance profile",
            profile: None,
            windows: false,
            registers: false,
            platform_type: "",
        },
        Case {
            case: "a role carrying the AWS-managed policy a client attaches for this",
            profile: Some("ssm-core"),
            windows: false,
            registers: true,
            platform_type: "Linux",
        },
        Case {
            case: "a role whose inline policy names the registration call",
            profile: Some("ssm-inline"),
            windows: false,
            registers: true,
            platform_type: "Linux",
        },
        Case {
            case: "a Windows instance whose role is granted every action of this service",
            profile: Some("ssm-wildcard"),
            windows: true,
            registers: true,
            platform_type: "Windows",
        },
    ]
}

/// An instance becomes a managed node exactly when the instance, its instance profile and
/// that profile's role say so, and the node reports the instance's own facts.
#[tokio::test]
async fn an_instance_registers_only_when_its_profile_lets_the_agent_register() {
    let router = emulator();
    for profile in profiles() {
        create_profile(&router, &profile).await;
    }
    let linux = image_id(&router, AL2023_PARAMETER).await;
    let windows = image_id(&router, WINDOWS_PARAMETER).await;

    let rows = registration_rows();
    let mut launched: Vec<(Case, String)> = Vec::new();
    for row in &rows {
        let image = if row.windows { &windows } else { &linux };
        let instance_id = launch(&router, row.case, REGION, image, row.profile).await;
        launched.push((*row, instance_id));
    }

    // The rows that register: the node is reported once the clock makes the registration
    // due, and reports what the instance holds.
    for (row, instance_id) in launched.iter().filter(|(row, _)| row.registers) {
        let reported = node_when_registered(&router, row.case, REGION, instance_id).await;
        for (member, expected) in [
            ("InstanceId", json!(instance_id)),
            ("PingStatus", json!("Online")),
            ("ResourceType", json!("EC2Instance")),
            ("PlatformType", json!(row.platform_type)),
            ("IsLatestVersion", json!(true)),
        ] {
            assert_eq!(
                reported[member], expected,
                "case {:?}: expected {member} {expected}, got {reported}",
                row.case
            );
        }
        for member in [
            "IPAddress",
            "ComputerName",
            "AgentVersion",
            "RegistrationDate",
            "LastPingDateTime",
        ] {
            assert!(
                !reported[member].is_null()
                    && reported[member].as_str().is_none_or(|held| !held.is_empty()),
                "case {:?}: expected a {member} for the registered node, got {reported}",
                row.case
            );
        }
    }

    // The rows that do not: nothing is reported for them, and the spans of all of them had
    // passed by the time the registrations above were observed.
    for (row, instance_id) in launched.iter().filter(|(row, _)| !row.registers) {
        let reported = node(&router, REGION, instance_id).await;
        assert!(
            reported.is_none(),
            "case {:?}: expected {instance_id} to be no managed node, got {reported:?}",
            row.case
        );
        assert!(
            inventory(&router, REGION, instance_id).await.is_none(),
            "case {:?}: expected no inventory for {instance_id}",
            row.case
        );
    }

    // A read reports the same registration however often it is taken: the registration
    // instant is derived from the launch, not from the clock of whoever read.
    let (row, instance_id) = launched
        .iter()
        .find(|(row, _)| row.registers)
        .expect("the table registers at least one node");
    let first = node_when_registered(&router, row.case, REGION, instance_id).await;
    let again = node_when_registered(&router, row.case, REGION, instance_id).await;
    assert_eq!(
        first["RegistrationDate"], again["RegistrationDate"],
        "case {:?}: two reads must report one RegistrationDate, got {first} then {again}",
        row.case
    );
}

/// A registered node is reported only through the endpoint of the Region its instance runs
/// in, and stops being reported once Amazon EC2 no longer holds that instance running.
#[tokio::test]
async fn a_node_is_regional_and_goes_with_its_instance() {
    const OTHER_REGION: &str = "eu-central-1";

    let router = emulator();
    let profile = Profile {
        name: "ssm-core",
        trusts: "ec2.amazonaws.com",
        grant: Grant::AwsManaged,
    };
    create_profile(&router, &profile).await;
    let image = image_id(&router, AL2023_PARAMETER).await;

    let here = launch(&router, "this Region", REGION, &image, Some(profile.name)).await;
    let elsewhere = launch(
        &router,
        "another Region",
        OTHER_REGION,
        &image,
        Some(profile.name),
    )
    .await;

    for (case, region, instance_id) in [
        ("this Region", REGION, &here),
        ("another Region", OTHER_REGION, &elsewhere),
    ] {
        node_when_registered(&router, case, region, instance_id).await;
        assert!(
            node(&router, region, if region == REGION { &elsewhere } else { &here })
                .await
                .is_none(),
            "case {case:?}: a node registered in another Region must not be reported here"
        );
    }

    // Amazon EC2 owns the instance's life: once it no longer holds it running, this service
    // reports no node for it — "this operation does not return information for nodes that
    // are either Stopped or Terminated".
    let (status, body) = ec2(
        &router,
        REGION,
        "TerminateInstances",
        &[("InstanceId.1", here.as_str())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"terminated\": TerminateInstances must answer, got {status} with {body}"
    );
    let until = Instant::now() + DEADLINE;
    loop {
        if node(&router, REGION, &here).await.is_none() {
            break;
        }
        assert!(
            Instant::now() < until,
            "case \"terminated\": expected {here} to stop being reported as a managed node"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
    // The node of the instance that is still running is untouched by the other's end.
    node_when_registered(&router, "another Region", OTHER_REGION, &elsewhere).await;
}

/// The inventory of a registered node is its own instance information, selected by the
/// inventory filters a client queries with.
#[tokio::test]
async fn the_inventory_of_a_registered_node_is_its_own_information() {
    let router = emulator();
    let profile = Profile {
        name: "ssm-core",
        trusts: "ec2.amazonaws.com",
        grant: Grant::AwsManaged,
    };
    create_profile(&router, &profile).await;
    let linux_image = image_id(&router, AL2023_PARAMETER).await;
    let windows_image = image_id(&router, WINDOWS_PARAMETER).await;

    let linux = launch(&router, "Linux", REGION, &linux_image, Some(profile.name)).await;
    let windows = launch(
        &router,
        "Windows",
        REGION,
        &windows_image,
        Some(profile.name),
    )
    .await;
    let linux_node = node_when_registered(&router, "Linux", REGION, &linux).await;
    node_when_registered(&router, "Windows", REGION, &windows).await;

    // Every attribute the item reports is the node's own, and the two nodes differ in the
    // one their platforms differ in.
    let item = inventory(&router, REGION, &linux)
        .await
        .unwrap_or_else(|| panic!("case \"Linux\": expected an inventory entity for {linux}"));
    for (attribute, expected) in [
        ("InstanceId", json!(linux)),
        ("ResourceType", json!("EC2Instance")),
        ("PlatformType", json!("Linux")),
        ("InstanceStatus", json!("Active")),
        ("IpAddress", linux_node["IPAddress"].clone()),
        ("ComputerName", linux_node["ComputerName"].clone()),
        ("AgentVersion", linux_node["AgentVersion"].clone()),
    ] {
        assert_eq!(
            item[attribute], expected,
            "case \"Linux\": expected {attribute} {expected}, got {item}"
        );
    }
    assert!(
        item["IpAddress"]
            .as_str()
            .is_some_and(|address| !address.is_empty()),
        "case \"Linux\": expected a non-empty IpAddress, got {item}"
    );

    // A filter over the inventory selects by what the nodes hold: the platform of each.
    for (case, platform, expected) in [
        ("the Linux platform", "Linux", vec![linux.clone()]),
        ("the Windows platform", "Windows", vec![windows.clone()]),
        ("a platform neither node runs", "MacOS", Vec::new()),
    ] {
        let (status, body) = call_in_region(
            &router,
            REGION,
            "GetInventory",
            &json!({
                "Filters": [{
                    "Key": format!("{INVENTORY_TYPE}.PlatformType"),
                    "Values": [platform],
                }],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetInventory must answer, got {status} with {body}"
        );
        let reported: Vec<String> = body["Entities"]
            .as_array()
            .map(|entities| {
                entities
                    .iter()
                    .filter_map(|entity| entity["Id"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the entities {expected:?}, got {body}"
        );
    }
}

/// An association created for a registered node runs on it: its run finishes on the clock
/// and the association reports `Success` over the nodes it reached, while one whose targets
/// select no registered node reports no execution at all.
#[tokio::test]
async fn an_association_runs_on_the_nodes_its_targets_select() {
    /// A well-formed node id this Region holds no node under.
    const UNREGISTERED: &str = "i-0dead999";

    let router = emulator();
    let profile = Profile {
        name: "ssm-core",
        trusts: "ec2.amazonaws.com",
        grant: Grant::AwsManaged,
    };
    create_profile(&router, &profile).await;
    let image = image_id(&router, AL2023_PARAMETER).await;
    let instance_id = launch(&router, "the inventory policy", REGION, &image, Some(profile.name))
        .await;
    node_when_registered(&router, "the inventory policy", REGION, &instance_id).await;

    // The document is the inventory policy Systems Manager publishes, associated the way a
    // client associates it: by target, with no parameters of its own.
    let created = create_association(&router, "a registered node", &instance_id).await;
    assert_eq!(
        created["Overview"]["Status"], json!("Pending"),
        "case \"a registered node\": a create answers before the run has finished, got {created}"
    );
    let association_id = created["AssociationId"]
        .as_str()
        .unwrap_or_else(|| panic!("a create reports an AssociationId, got {created}"))
        .to_string();

    // An association whose target selects no registered node: created the same way, and
    // dispatched to nothing.
    let other = create_association(&router, "an unregistered node", UNREGISTERED).await;
    let other_id = other["AssociationId"]
        .as_str()
        .expect("a create reports an AssociationId")
        .to_string();

    let succeeded = association_when_successful(&router, "a registered node", &association_id).await;
    assert_eq!(
        succeeded["Overview"]["DetailedStatus"], json!("Success"),
        "case \"a registered node\": expected the detail of a finished run, got {succeeded}"
    );
    assert_eq!(
        succeeded["Overview"]["AssociationStatusAggregatedCount"], json!({ "Success": 1 }),
        "case \"a registered node\": expected one target counted as a success, got {succeeded}"
    );
    let ran_at = succeeded["LastExecutionDate"].clone();
    assert!(
        !ran_at.is_null(),
        "case \"a registered node\": expected a LastExecutionDate, got {succeeded}"
    );
    assert_eq!(
        succeeded["LastSuccessfulExecutionDate"], ran_at,
        "case \"a registered node\": a run that succeeded is the last successful one, got \
         {succeeded}"
    );

    // The association that reached no node settles too: its execution is over as soon as
    // the targets have been evaluated, and it reports the result of a run over no node —
    // `Success`, with no count of targets by status, because there were no targets.
    //
    // Its settle is a span of ITS OWN, so this looks again for it — the helper returns only
    // once the association reports `Success`, and fails naming the body otherwise — instead
    // of resting on however long the run above happened to take: on a loaded host the two
    // spans need not have elapsed in the same order they fell due.
    let reached_nothing =
        association_when_successful(&router, "an unregistered node", &other_id).await;
    assert_eq!(
        reached_nothing["Overview"]["DetailedStatus"], json!("Success"),
        "case \"an unregistered node\": expected the detail of a finished run, got \
         {reached_nothing}"
    );
    assert!(
        reached_nothing["Overview"]["AssociationStatusAggregatedCount"].is_null(),
        "case \"an unregistered node\": expected no count of targets by status, got \
         {reached_nothing}"
    );

    // `StartAssociationsOnce` runs it again: a second run finishes on the clock too, and
    // the association then reports THAT run.
    let (status, body) = call_in_region(
        &router,
        REGION,
        "StartAssociationsOnce",
        &json!({ "AssociationIds": [association_id] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"a second run\": StartAssociationsOnce must answer, got {status} with {body}"
    );
    let until = Instant::now() + DEADLINE;
    loop {
        let held = describe_association(&router, &association_id).await;
        if held["Overview"]["Status"] == json!("Success")
            && held["LastExecutionDate"] != ran_at
        {
            break;
        }
        assert!(
            Instant::now() < until,
            "case \"a second run\": expected a run later than {ran_at} to finish, got {held}"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }

    // An id this Region holds no association under is refused, and nothing is dispatched
    // for the request: a run starts for every association it names or for none.
    let before = describe_association(&router, &other_id).await;
    let (status, body) = call_in_region(
        &router,
        REGION,
        "StartAssociationsOnce",
        &json!({ "AssociationIds": [other_id, "00000000-0000-0000-0000-000000000000"] }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"an id nothing is held under\": expected a refusal, got {status} with {body}"
    );
    assert_eq!(
        error_code(&body),
        "AssociationDoesNotExist",
        "case \"an id nothing is held under\": expected AssociationDoesNotExist, got {body}"
    );
    let after = describe_association(&router, &other_id).await;
    assert_eq!(
        after["Overview"], before["Overview"],
        "case \"an id nothing is held under\": the association named beside it must be \
         untouched, got {after}"
    );
}

/// Create an association of the published inventory policy document for the node
/// `instance_id`, as the `AssociationDescription` the create answered.
async fn create_association(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> Value {
    let (status, body) = call_in_region(
        router,
        REGION,
        "CreateAssociation",
        &json!({
            "Name": "AWS-GatherSoftwareInventory",
            "Targets": [{ "Key": "InstanceIds", "Values": [instance_id] }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAssociation must succeed, got {status} with {body}"
    );
    body["AssociationDescription"].clone()
}

/// The `AssociationDescription` this service reports for `association_id`.
async fn describe_association(router: &MultiServiceRouter, association_id: &str) -> Value {
    let (status, body) = call_in_region(
        router,
        REGION,
        "DescribeAssociation",
        &json!({ "AssociationId": association_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeAssociation({association_id}) must answer, got {status} with {body}"
    );
    body["AssociationDescription"].clone()
}

/// Look again until the association `association_id` reports a run that succeeded, or
/// [`DEADLINE`] passes.
async fn association_when_successful(
    router: &MultiServiceRouter,
    case: &str,
    association_id: &str,
) -> Value {
    let until = Instant::now() + DEADLINE;
    loop {
        let held = describe_association(router, association_id).await;
        if held["Overview"]["Status"] == json!("Success") {
            return held;
        }
        assert!(
            Instant::now() < until,
            "case {case:?}: expected the association's run to finish, got {held}"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// Launch one instance carrying `tags`, through the `TagSpecifications` of Amazon EC2's own
/// `RunInstances` ("the tags to apply to the resources that are created during instance
/// launch"), as the instance id that service answered.
///
/// Every key and value is free of `&`, `=`, `+`, `%` and spaces, so it travels in the form
/// body as written, the way every other value these cases send does.
async fn launch_tagged(
    router: &MultiServiceRouter,
    case: &str,
    image: &str,
    profile: &str,
    tags: &[(&str, &str)],
) -> String {
    let mut parameters: Vec<(String, String)> = vec![
        ("ImageId".to_string(), image.to_string()),
        ("MinCount".to_string(), "1".to_string()),
        ("MaxCount".to_string(), "1".to_string()),
        (
            "IamInstanceProfile.Name".to_string(),
            profile.to_string(),
        ),
        (
            "TagSpecification.1.ResourceType".to_string(),
            "instance".to_string(),
        ),
    ];
    for (index, (key, value)) in tags.iter().enumerate() {
        let entry = index + 1;
        parameters.push((
            format!("TagSpecification.1.Tag.{entry}.Key"),
            (*key).to_string(),
        ));
        parameters.push((
            format!("TagSpecification.1.Tag.{entry}.Value"),
            (*value).to_string(),
        ));
    }
    let borrowed: Vec<(&str, &str)> = parameters
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let (status, body) = ec2(router, REGION, "RunInstances", &borrowed).await;
    assert_eq!(
        status, 200,
        "case {case:?}: RunInstances must launch, got {status} with {body}"
    );
    element(&body, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: RunInstances reports an instance id: {body}"))
}

/// The tags this service reports for the resource `instance_id` names, keyed by tag key.
async fn node_tags(router: &MultiServiceRouter, case: &str, instance_id: &str) -> Vec<(String, String)> {
    let (status, body) = call_in_region(
        router,
        REGION,
        "ListTagsForResource",
        &json!({ "ResourceType": "ManagedInstance", "ResourceId": instance_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListTagsForResource must answer, got {status} with {body}"
    );
    let mut held: Vec<(String, String)> = body["TagList"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|tag| {
            (
                tag["Key"].as_str().unwrap_or_default().to_string(),
                tag["Value"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    held.sort();
    held
}

/// RULE: a managed node registered from an Amazon EC2 instance IS that instance, so the tags
/// it carries are the instance's own — which is what a `tag:` target selects it by and what
/// Patch Manager reads its patch group from. An instance tagged after its agent registered
/// carries that tag too.
#[tokio::test]
async fn a_registered_node_carries_the_tags_of_its_instance() {
    let router = emulator();
    let profile = Profile {
        name: "patch-manager-node",
        trusts: EC2_PRINCIPAL,
        grant: Grant::Inline(GRANTS_REGISTRATION),
    };
    create_profile(&router, &profile).await;
    let image = image_id(&router, AL2023_PARAMETER).await;

    let case = "an instance tagged at launch";
    let instance = launch_tagged(
        &router,
        case,
        &image,
        profile.name,
        &[("PatchGroup", "production"), ("ServerRole", "web")],
    )
    .await;
    node_when_registered(&router, case, REGION, &instance).await;

    assert_eq!(
        node_tags(&router, case, &instance).await,
        vec![
            ("PatchGroup".to_string(), "production".to_string()),
            ("ServerRole".to_string(), "web".to_string()),
        ],
        "case {case:?}: the node carries the tags the launch put on the instance"
    );

    // A tag target selects the node by one of those tags, and selects nothing by a value the
    // instance does not carry.
    for (tag, value, selects) in [
        ("tag:ServerRole", "web", true),
        ("tag:PatchGroup", "production", true),
        ("tag:ServerRole", "database", false),
    ] {
        let (status, sent) = call_in_region(
            &router,
            REGION,
            "SendCommand",
            &json!({
                "DocumentName": "AWS-RunPatchBaseline",
                "Targets": [{ "Key": tag, "Values": [value] }],
                "Parameters": { "Operation": ["Scan"] },
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the send must be served for {tag}={value}, got {status} with {sent}"
        );
        assert_eq!(
            sent["Command"]["TargetCount"].as_i64(),
            Some(i64::from(selects)),
            "case {case:?}: {tag}={value} must select {} node, got {sent}",
            i64::from(selects)
        );
    }

    // The patch run that reached the node recorded the patch group its tag names, which is
    // what the patch-group read aggregates over.
    let (status, states) = call_in_region(
        &router,
        REGION,
        "DescribeInstancePatchStates",
        &json!({ "InstanceIds": [instance] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the patch-state read must answer, got {status} with {states}"
    );
    assert_eq!(
        states["InstancePatchStates"][0]["PatchGroup"].as_str(),
        Some("production"),
        "case {case:?}: the report carries the patch group the instance's tag names, got \
         {states}"
    );
    let (status, group) = call_in_region(
        &router,
        REGION,
        "DescribePatchGroupState",
        &json!({ "PatchGroup": "production" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the patch-group read must answer, got {status} with {group}"
    );
    assert_eq!(
        group["Instances"].as_i64(),
        Some(1),
        "case {case:?}: the group holds the node whose tag names it, got {group}"
    );

    // A tag put on the instance AFTER its agent registered reaches the node too: the tags
    // are the instance's, brought in line whenever this service settles its registrations,
    // not a copy frozen at registration.
    let case = "an instance tagged after it registered";
    let (status, tagged) = ec2(
        &router,
        REGION,
        "CreateTags",
        &[
            ("ResourceId.1", instance.as_str()),
            ("Tag.1.Key", "Environment"),
            ("Tag.1.Value", "staging"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateTags must succeed, got {status} with {tagged}"
    );
    let (status, sent) = call_in_region(
        &router,
        REGION,
        "SendCommand",
        &json!({
            "DocumentName": "AWS-RunPatchBaseline",
            "Targets": [{ "Key": "tag:Environment", "Values": ["staging"] }],
            "Parameters": { "Operation": ["Scan"] },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the send must be served, got {status} with {sent}"
    );
    assert_eq!(
        sent["Command"]["TargetCount"].as_i64(),
        Some(1),
        "case {case:?}: the new tag selects the node, got {sent}"
    );
    assert!(
        node_tags(&router, case, &instance)
            .await
            .contains(&("Environment".to_string(), "staging".to_string())),
        "case {case:?}: the node carries the tag the instance was given"
    );
}
