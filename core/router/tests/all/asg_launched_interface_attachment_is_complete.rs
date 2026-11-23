// SPDX-License-Identifier: CC0-1.0

//! The ATTACHMENT STATE of the network interfaces an instance an Auto Scaling group launched
//! holds, read through the whole mounted service set — because the rule only holds if Amazon
//! EC2 Auto Scaling and Amazon EC2 agree over one engine: the group makes the launch, and the
//! interface is Amazon EC2's own record, read back through `DescribeInstances` and
//! `DescribeNetworkInterfaces` the way a client reads it.
//!
//! The RULE under test: an attachment reaches its terminal `attached` state by the time the
//! FIRST client read can see it whenever no response ever reported it being made, and only an
//! attachment a caller's own call was answered with sits in the transient `attaching` state
//! until the clock carries it — `Attachment.Status` is "the attachment state"
//! (`smithy_query(query="shape",shape="NetworkInterfaceAttachment")`, whose `Status` targets
//! `AttachmentStatus`, the enum `attaching | attached | detaching | detached`). Amazon EC2
//! Auto Scaling launches each instance itself, so nothing answers a client the attachment of
//! that instance's interfaces being made and the first read of one — by instance or by
//! interface id — finds it finished; a caller's own `RunInstances` IS answered the attachment
//! in flight, so that one is `attaching` in the launch's own reply and reaches `attached` on
//! Amazon EC2's clock.
//!
//! The rows span what the rule generalises over: HOW the group reached its capacity (declared
//! on the create, raised from zero by `UpdateAutoScalingGroup`, raised from zero by
//! `SetDesiredCapacity`), HOW MANY instances one capacity change launches (one, two), WHAT the
//! launch-template version declares about its interfaces and groups (nothing at all, its own
//! security group ids, its primary network interface spelled out), and WHERE the group names
//! that template (its own `LaunchTemplate`, or the launch template of a
//! `MixedInstancesPolicy`). The last leg is the other half of the rule: the same emulator's
//! `RunInstances`, whose own answer reports the attachment being made.
//!
//! Every id asserted is one the services minted — the VPC, subnet and security group through
//! Amazon EC2's own calls, the instance ids off the group's own report, the interface ids off
//! the instance's — so no value here is composed by the suite, and the attachment state is
//! asserted on the FIRST read of each interface rather than polled for, since a read that
//! waits would not tell a finished attachment from one the clock later finished.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for query suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{block_of, blocks_of, element, elements, opt_element};

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The API versions an ec2Query and an awsQuery request here carries.
const EC2_API_VERSION: &str = "2016-11-15";
const ASG_VERSION: &str = "2011-01-01";

/// The AMI, instance type and Availability Zone every launch here names.
const IMAGE_ID: &str = "ami-0abcdef1234567890";
const INSTANCE_TYPE: &str = "t3.micro";
const ZONE: &str = "us-east-1a";

/// The device index of the interface every instance in a VPC has: "the primary network
/// interface has a device index of 0"
/// (`smithy_query(query="doc",shape="NetworkInterfaceAttachment",member="DeviceIndex")` and the
/// launch-template specification's own `DeviceIndex`).
const PRIMARY_DEVICE_INDEX: &str = "0";

/// The prefix Amazon EC2 gives the id of a network interface attachment, which is the shape of
/// `NetworkInterfaceAttachment$AttachmentId` rather than any one run's value.
const ATTACHMENT_ID_PREFIX: &str = "eni-attach-";

/// The terminal and the transient member of `AttachmentStatus`, the enum
/// `NetworkInterfaceAttachment$Status` targets.
const ATTACHED: &str = "attached";
const ATTACHING: &str = "attaching";

/// Every value `AttachmentStatus` has, from
/// `smithy_query(query="shape",shape="AttachmentStatus")`: a status outside this set is not a
/// state an attachment can be reported in at all.
const ATTACHMENT_STATUS_VALUES: &[&str] = &["attaching", "attached", "detaching", "detached"];

/// The status Amazon EC2 reports for an interface something holds
/// (`NetworkInterface$Status`).
const IN_USE: &str = "in-use";

/// How long the last leg waits for Amazon EC2's own clock to finish the attachment a caller's
/// `RunInstances` was answered in flight, and how often it looks. The deadline is generous and
/// only there so a failure is a failure rather than a hang: the leg waits for the CONDITION,
/// never for a fixed time.
const DEADLINE: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(50);

/// The whole emulator over one engine, exactly as the binary assembles it.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's.
    router.set_request_authentication(false);
    router
}

/// One form body: the action, the version and the parameters.
fn form(action: &str, version: &str, params: &[(String, String)]) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&crate::query_wire::form_encode(name));
        body.push('=');
        body.push_str(&crate::query_wire::form_encode(value));
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

/// Send one form-encoded request to the service `signing_name` addresses.
async fn send(router: &MultiServiceRouter, signing_name: &str, body: String) -> (u16, String) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.{REGION}.amazonaws.com/"))
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One Amazon EC2 action that must succeed.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(String, String)]) -> String {
    let (status, body) = send(router, "ec2", form(action, EC2_API_VERSION, params)).await;
    assert_eq!(
        status, 200,
        "EC2 {action} must succeed, got {status} with body {body}"
    );
    body
}

/// One Auto Scaling action that must succeed.
async fn asg(router: &MultiServiceRouter, action: &str, params: &[(String, String)]) -> String {
    let (status, body) = send(router, "autoscaling", form(action, ASG_VERSION, params)).await;
    assert_eq!(
        status, 200,
        "Auto Scaling {action} must succeed, got {status} with body {body}"
    );
    body
}

/// The network every group and every launch here places its instances in.
struct Network {
    subnet: String,
    /// One security group of that VPC, which a row's version may declare as its own.
    group: String,
}

async fn seed_network(router: &MultiServiceRouter) -> Network {
    let vpc = element(
        &ec2(router, "CreateVpc", &[p("CidrBlock", "10.0.0.0/16")]).await,
        "vpcId",
    );
    let subnet = element(
        &ec2(
            router,
            "CreateSubnet",
            &[
                p("VpcId", vpc.clone()),
                p("CidrBlock", "10.0.0.0/18"),
                p("AvailabilityZone", ZONE),
            ],
        )
        .await,
        "subnetId",
    );
    let group = element(
        &ec2(
            router,
            "CreateSecurityGroup",
            &[
                p("GroupName", "asg-eni-attachment"),
                p("GroupDescription", "a group a launch template names"),
                p("VpcId", vpc.clone()),
            ],
        )
        .await,
        "groupId",
    );
    Network { subnet, group }
}

/// What a row's launch-template version declares about the interfaces and groups of its
/// instances.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Declares {
    /// Nothing: the version names neither a group nor an interface, so Amazon EC2 gives the
    /// instance the one primary interface every instance in a VPC has.
    Nothing,
    /// Its own `SecurityGroupIds`, applied to that same primary interface.
    ItsOwnSecurityGroups,
    /// Its primary network interface spelled out, at the device index the primary one has.
    ItsPrimaryInterface,
}

impl Declares {
    /// The `LaunchTemplateData` parameters this declaration is carried by.
    fn version_data(self, network: &Network) -> Vec<(String, String)> {
        match self {
            Self::Nothing => Vec::new(),
            Self::ItsOwnSecurityGroups => {
                vec![data("SecurityGroupId.1", network.group.clone())]
            }
            Self::ItsPrimaryInterface => vec![
                data("NetworkInterface.1.DeviceIndex", PRIMARY_DEVICE_INDEX),
                data("NetworkInterface.1.SecurityGroupId.1", network.group.clone()),
            ],
        }
    }
}

/// Where the group names its launch template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// The group's own `LaunchTemplate`.
    Own,
    /// The launch template of its `MixedInstancesPolicy`.
    MixedPolicy,
}

impl Source {
    /// The parameter the template's id is carried under.
    fn member(self) -> &'static str {
        match self {
            Self::Own => "LaunchTemplate.LaunchTemplateId",
            Self::MixedPolicy => "MixedInstancesPolicy.LaunchTemplate.\
                                  LaunchTemplateSpecification.LaunchTemplateId",
        }
    }
}

/// How a row's group reaches the capacity it launches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reached {
    /// Declared on the create.
    OnTheCreate,
    /// Zero on the create, raised by `UpdateAutoScalingGroup`.
    ByUpdateAutoScalingGroup,
    /// Zero on the create, raised by `SetDesiredCapacity` alone.
    BySetDesiredCapacity,
}

/// Create a launch template whose first version carries `version_data`, and answer its id.
async fn create_template(
    router: &MultiServiceRouter,
    name: &str,
    version_data: &[(String, String)],
) -> String {
    let mut params = vec![
        p("LaunchTemplateName", name),
        data("ImageId", IMAGE_ID),
        data("InstanceType", INSTANCE_TYPE),
        // What the recorded client asked for, and what a hardened launch names: an instance
        // whose metadata service takes a session token only.
        data("MetadataOptions.HttpTokens", "required"),
    ];
    params.extend_from_slice(version_data);
    element(
        &ec2(router, "CreateLaunchTemplate", &params).await,
        "launchTemplateId",
    )
}

/// The instance ids the group `name` reports.
async fn group_instances(router: &MultiServiceRouter, name: &str) -> Vec<String> {
    let body = asg(
        router,
        "DescribeAutoScalingGroups",
        &[p("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    elements(&body, "InstanceId")
}

/// Hold the Auto Scaling group `row` describes under `name`, launching from
/// `launch_template_id` into `subnet`, and answer the instance ids it reports.
///
/// The ids come off the group's OWN report, and are read once rather than polled for: a
/// capacity change launches its instances as it is applied, so a group that reports fewer
/// instances than it was asked for is a failure of this leg rather than something to wait out.
/// A row that starts at zero is asserted to hold NOTHING before its capacity call, so the
/// instances it then reports are attributable to that call rather than to the create.
async fn launched_instances(
    router: &MultiServiceRouter,
    row: &Row,
    name: &str,
    subnet: &str,
    launch_template_id: &str,
) -> Vec<String> {
    let case = row.case;
    let wanted = row.instances.to_string();
    let starts_at = match row.reached {
        Reached::OnTheCreate => wanted.clone(),
        Reached::ByUpdateAutoScalingGroup | Reached::BySetDesiredCapacity => "0".to_string(),
    };
    asg(
        router,
        "CreateAutoScalingGroup",
        &[
            p("AutoScalingGroupName", name),
            p("MinSize", "0"),
            p("MaxSize", wanted.clone()),
            p("DesiredCapacity", starts_at),
            p(row.source.member(), launch_template_id),
            p("VPCZoneIdentifier", subnet),
        ],
    )
    .await;
    match row.reached {
        Reached::OnTheCreate => {}
        Reached::ByUpdateAutoScalingGroup | Reached::BySetDesiredCapacity => {
            let before = group_instances(router, name).await;
            assert!(
                before.is_empty(),
                "case {case:?}: a group created at zero capacity launches nothing, so the \
                 instances below are the capacity call's own, got {before:?}"
            );
            if row.reached == Reached::ByUpdateAutoScalingGroup {
                asg(
                    router,
                    "UpdateAutoScalingGroup",
                    &[
                        p("AutoScalingGroupName", name),
                        p("MinSize", wanted.clone()),
                        p("MaxSize", wanted.clone()),
                        p("DesiredCapacity", wanted.clone()),
                    ],
                )
                .await;
            } else {
                asg(
                    router,
                    "SetDesiredCapacity",
                    &[
                        p("AutoScalingGroupName", name),
                        p("DesiredCapacity", wanted.clone()),
                    ],
                )
                .await;
            }
        }
    }
    let reported = group_instances(router, name).await;
    assert_eq!(
        reported.len(),
        row.instances,
        "case {case:?}: expected the group to report {} instance(s), got {reported:?}",
        row.instances
    );
    reported
}

/// One attachment as Amazon EC2 reports it, reduced to the members this rule reads.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Attachment {
    status: String,
    instance_id: String,
    instance_owner_id: String,
    attachment_id: String,
    attach_time: String,
    device_index: String,
    network_card_index: String,
    delete_on_termination: String,
}

/// One interface as `DescribeNetworkInterfaces` reports it for ONE id: its own status and its
/// attachment.
///
/// Read one interface at a time on purpose: an ec2Query answer nests `<item>` lists inside each
/// interface (its groups, its addresses), so a reader over the whole answer would tear one
/// interface into several. The interface's OWN `status` is read with the attachment block cut
/// out, which carries a member of that name too, so the read does not depend on which side of
/// the attachment the serializer writes it.
async fn described_interface(
    router: &MultiServiceRouter,
    case: &str,
    network_interface_id: &str,
) -> (String, Attachment) {
    let body = ec2(
        router,
        "DescribeNetworkInterfaces",
        &[p("NetworkInterfaceId.1", network_interface_id)],
    )
    .await;
    let item = block_of(&body, "networkInterfaceSet").unwrap_or_else(|| {
        panic!("case {case:?}: no <networkInterfaceSet> in the answer for {network_interface_id}: {body}")
    });
    assert_eq!(
        opt_element(item, "networkInterfaceId").as_deref(),
        Some(network_interface_id),
        "case {case:?}: expected the answer to report {network_interface_id}, got {item}"
    );
    let attachment = block_of(item, "attachment").unwrap_or_else(|| {
        panic!("case {case:?}: {network_interface_id} reports no attachment at all in {item}")
    });
    // The item with its attachment block cut out: what is left is the interface's own, which
    // is where its own `status` is read from, whichever side of the attachment it is written
    // on.
    let (head, inside) = item.split_once("<attachment").unwrap_or((item, ""));
    let tail = inside
        .split_once("</attachment>")
        .map_or("", |(_, rest)| rest);
    let own = format!("{head}{tail}");
    (
        opt_element(&own, "status").unwrap_or_default(),
        Attachment {
            status: opt_element(attachment, "status").unwrap_or_default(),
            instance_id: opt_element(attachment, "instanceId").unwrap_or_default(),
            instance_owner_id: opt_element(attachment, "instanceOwnerId").unwrap_or_default(),
            attachment_id: opt_element(attachment, "attachmentId").unwrap_or_default(),
            attach_time: opt_element(attachment, "attachTime").unwrap_or_default(),
            device_index: opt_element(attachment, "deviceIndex").unwrap_or_default(),
            network_card_index: opt_element(attachment, "networkCardIndex").unwrap_or_default(),
            delete_on_termination: opt_element(attachment, "deleteOnTermination")
                .unwrap_or_default(),
        },
    )
}

/// What one instance's own report says about its interfaces: the ids it names, and the state of
/// every attachment it reports.
///
/// The two are read as SETS of the same block rather than zipped together: an interface entry
/// that ever omitted one of them would misalign the pairing instead of failing, and the rule
/// here is about every attachment the instance reports, whichever interface carries it.
struct InstanceView {
    interface_ids: BTreeSet<String>,
    attachment_statuses: Vec<String>,
}

async fn instance_view(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> InstanceView {
    let body = ec2(
        router,
        "DescribeInstances",
        &[p("InstanceId.1", instance_id)],
    )
    .await;
    assert!(
        body.contains(instance_id),
        "case {case:?}: Amazon EC2 must hold the instance {instance_id} the group launched, \
         got {body}"
    );
    let set = block_of(&body, "networkInterfaceSet").unwrap_or_else(|| {
        panic!("case {case:?}: the instance {instance_id} reports no networkInterfaceSet: {body}")
    });
    InstanceView {
        interface_ids: elements(set, "networkInterfaceId").into_iter().collect(),
        attachment_statuses: blocks_of(set, "attachment")
            .into_iter()
            .map(|attachment| opt_element(attachment, "status").unwrap_or_default())
            .collect(),
    }
}

/// One case.
struct Row {
    case: &'static str,
    reached: Reached,
    instances: usize,
    declares: Declares,
    source: Source,
}

const ROWS: &[Row] = &[
    Row {
        case: "a group created at the capacity it launches",
        reached: Reached::OnTheCreate,
        instances: 1,
        declares: Declares::Nothing,
        source: Source::Own,
    },
    Row {
        case: "a group raised from zero by UpdateAutoScalingGroup",
        reached: Reached::ByUpdateAutoScalingGroup,
        instances: 1,
        declares: Declares::Nothing,
        source: Source::Own,
    },
    Row {
        case: "a group raised from zero by SetDesiredCapacity",
        reached: Reached::BySetDesiredCapacity,
        instances: 1,
        declares: Declares::Nothing,
        source: Source::Own,
    },
    Row {
        case: "two instances launched by one capacity change",
        reached: Reached::OnTheCreate,
        instances: 2,
        declares: Declares::Nothing,
        source: Source::Own,
    },
    Row {
        case: "a version declaring its own security groups",
        reached: Reached::ByUpdateAutoScalingGroup,
        instances: 1,
        declares: Declares::ItsOwnSecurityGroups,
        source: Source::Own,
    },
    Row {
        case: "a version declaring its primary network interface",
        reached: Reached::OnTheCreate,
        instances: 1,
        declares: Declares::ItsPrimaryInterface,
        source: Source::Own,
    },
    Row {
        case: "a group launching through a mixed instances policy",
        reached: Reached::BySetDesiredCapacity,
        instances: 1,
        declares: Declares::Nothing,
        source: Source::MixedPolicy,
    },
];

/// RULE: every interface an instance an Auto Scaling group launched holds reports a FINISHED
/// attachment the first time a client can read it — by interface id and in the instance's own
/// report alike — because nothing ever answered a client that attachment being made.
#[tokio::test]
async fn an_interface_of_a_group_launched_instance_reads_attached_on_its_first_read() {
    let router = router();
    let network = seed_network(&router).await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let template = create_template(
            &router,
            &format!("asg-eni-attach-lt-{index}"),
            &row.declares.version_data(&network),
        )
        .await;
        let instances = launched_instances(
            &router,
            row,
            &format!("asg-eni-attach-{index}"),
            &network.subnet,
            &template,
        )
        .await;

        for instance_id in &instances {
            let view = instance_view(&router, case, instance_id).await;
            assert!(
                !view.interface_ids.is_empty(),
                "case {case:?}: the instance {instance_id} must report the interface its launch \
                 gave it, got none"
            );
            assert_eq!(
                view.attachment_statuses.len(),
                view.interface_ids.len(),
                "case {case:?}: the instance {instance_id} must report one attachment per \
                 interface ({:?}), got {:?}",
                view.interface_ids,
                view.attachment_statuses
            );
            // The instance's OWN view of each attachment: finished, every one of them.
            for status in &view.attachment_statuses {
                assert_eq!(
                    status, ATTACHED,
                    "case {case:?}: the instance {instance_id} reports an attachment in \
                     {status:?}; a launch nothing was answered for leaves none in flight, got \
                     {:?}",
                    view.attachment_statuses
                );
            }
            // …and the independent read of each interface by id, which is the read a waiter,
            // an ENI-health check or a secondary-interface tool makes.
            let mut by_device_index: BTreeMap<String, String> = BTreeMap::new();
            for network_interface_id in &view.interface_ids {
                let (interface_status, attachment) =
                    described_interface(&router, case, network_interface_id).await;
                assert_eq!(
                    (attachment.status.as_str(), attachment.instance_id.as_str()),
                    (ATTACHED, instance_id.as_str()),
                    "case {case:?}: {network_interface_id} must read attached to {instance_id} \
                     on its first read, got {attachment:?}"
                );
                assert_eq!(
                    interface_status, IN_USE,
                    "case {case:?}: an interface {instance_id} holds is in use, got \
                     {interface_status:?} for {network_interface_id}"
                );
                // The identity of a finished attachment: an id of the service's own shape, the
                // instant it was made, the account that owns the instance, and the index the
                // interface is attached at.
                assert!(
                    attachment
                        .attachment_id
                        .starts_with(ATTACHMENT_ID_PREFIX)
                        && attachment.attachment_id.len() > ATTACHMENT_ID_PREFIX.len()
                        && !attachment.attach_time.is_empty()
                        && !attachment.instance_owner_id.is_empty(),
                    "case {case:?}: {network_interface_id} must report a complete attachment \
                     identity, got {attachment:?}"
                );
                assert!(
                    attachment.device_index.parse::<i64>().is_ok(),
                    "case {case:?}: {network_interface_id} must report a numeric device index, \
                     got {attachment:?}"
                );
                let clash = by_device_index
                    .insert(attachment.device_index.clone(), network_interface_id.clone());
                assert!(
                    clash.is_none(),
                    "case {case:?}: two interfaces of {instance_id} report device index {:?} \
                     ({network_interface_id} and {clash:?})",
                    attachment.device_index
                );
                if attachment.device_index == PRIMARY_DEVICE_INDEX {
                    // "When you launch an instance, the primary network interface is deleted on
                    // termination" — the attachment of the interface the launch itself created.
                    assert_eq!(
                        (
                            attachment.delete_on_termination.as_str(),
                            attachment.network_card_index.as_str()
                        ),
                        ("true", "0"),
                        "case {case:?}: the primary interface {network_interface_id} of \
                         {instance_id} goes with its instance on card 0, got {attachment:?}"
                    );
                }
            }
            assert!(
                by_device_index.contains_key(PRIMARY_DEVICE_INDEX),
                "case {case:?}: the instance {instance_id} must hold the primary interface every \
                 instance in a VPC has, got {by_device_index:?}"
            );
        }
    }
}

/// RULE, the other half: an attachment a CALLER's own call was answered with is reported in
/// flight — `attaching` in that call's own reply — and reaches `attached` on Amazon EC2's clock
/// with no further call from anybody.
///
/// The transient state is asserted on the launch's OWN answer, so nothing here depends on how
/// fast the host runs, and the completion is waited for by LOOKING AGAIN rather than by
/// sleeping a span.
#[tokio::test]
async fn an_interface_of_a_callers_own_launch_is_attaching_until_the_clock_finishes_it() {
    let router = router();
    let network = seed_network(&router).await;
    let case = "a caller's own RunInstances";

    let launched = ec2(
        &router,
        "RunInstances",
        &[
            p("ImageId", IMAGE_ID),
            p("InstanceType", INSTANCE_TYPE),
            p("SubnetId", network.subnet.clone()),
            p("MinCount", "1"),
            p("MaxCount", "1"),
        ],
    )
    .await;
    let instance_id = element(&launched, "instanceId");
    let set = block_of(&launched, "networkInterfaceSet").unwrap_or_else(|| {
        panic!("case {case:?}: the launch must report the interface it attached: {launched}")
    });
    let network_interface_id = element(set, "networkInterfaceId");
    let answered: Vec<String> = blocks_of(set, "attachment")
        .into_iter()
        .map(|attachment| opt_element(attachment, "status").unwrap_or_default())
        .collect();
    assert_eq!(
        answered,
        vec![ATTACHING.to_string()],
        "case {case:?}: the launch's own answer reports the attachment it is making, got \
         {answered:?} in {launched}"
    );

    // And the clock, not a caller, finishes it: the interface is looked at again until it has.
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (interface_status, attachment) =
            described_interface(&router, case, &network_interface_id).await;
        // Whichever side of the move this read fell on, the state it reports is one the enum
        // has: an attachment is never reported in a state that is no member of it.
        assert!(
            ATTACHMENT_STATUS_VALUES.contains(&attachment.status.as_str()),
            "case {case:?}: {network_interface_id} reports {:?}, which is no member of \
             AttachmentStatus {ATTACHMENT_STATUS_VALUES:?}",
            attachment.status
        );
        if attachment.status == ATTACHED {
            assert_eq!(
                (attachment.instance_id.as_str(), interface_status.as_str()),
                (instance_id.as_str(), IN_USE),
                "case {case:?}: the settled attachment of {network_interface_id} is to \
                 {instance_id} on an in-use interface, got {attachment:?} / {interface_status:?}"
            );
            break;
        }
        assert_eq!(
            attachment.status, ATTACHING,
            "case {case:?}: an attachment being made is either still attaching or attached, got \
             {attachment:?}"
        );
        assert!(
            Instant::now() < deadline,
            "case {case:?}: Amazon EC2's clock never finished the attachment of \
             {network_interface_id}, last read {attachment:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}
