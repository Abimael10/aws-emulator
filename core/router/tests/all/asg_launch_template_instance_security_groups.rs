// SPDX-License-Identifier: CC0-1.0

//! The SECURITY GROUPS an instance an Auto Scaling group launched comes up in — through the
//! whole mounted service set, because the rule only holds if Amazon EC2 Auto Scaling and
//! Amazon EC2 agree over one engine: the group makes the launch, and the instance (and the
//! groups applied to it) is Amazon EC2's own record, read back through `DescribeInstances`
//! the way a client reads it.
//!
//! The RULE under test: a launch names the security groups of its instance in ONE of two
//! places, never both — the launch-template version's own `SecurityGroupIds` /
//! `SecurityGroups`, or the `Groups` of the network interfaces the version declares, since
//! "if you specify a network interface, you must specify any security groups as part of the
//! network interface instead of using this parameter"
//! (`smithy_query(query="doc",shape="RequestLaunchTemplateData",member="SecurityGroupIds")`)
//! — and an instance's groups are the ones applied to its PRIMARY network interface, the
//! entry at device index 0 ("the device index for the network interface attachment. The
//! primary network interface has a device index of 0",
//! `smithy_query(query="doc",shape="LaunchTemplateInstanceNetworkInterfaceSpecificationRequest",member="DeviceIndex")`),
//! which is the index an entry that names none takes. Only a version that declares no group
//! at all leaves its instances in the VPC's default group ("Default: Amazon Web Services
//! uses the default security group", `RunInstancesRequest$SecurityGroupIds`).
//!
//! The rows span what the rule generalises over: WHERE the version declares its groups (on
//! the primary interface, on its own id list, on its own NAME list, nowhere), HOW MANY it
//! declares there (one, two), the ORDER the interfaces are listed in (the primary declared
//! after a secondary one, each with groups of its own), and WHERE the group names the
//! template (its own `LaunchTemplate`, or the launch template of a `MixedInstancesPolicy`).
//!
//! Every id asserted is one Amazon EC2 minted — the groups through `CreateSecurityGroup`,
//! the VPC's default group through EC2's own filters — so no value here is composed by the
//! suite.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for query suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, elements};

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The API versions an ec2Query and an awsQuery request here carries.
const EC2_API_VERSION: &str = "2016-11-15";
const ASG_VERSION: &str = "2011-01-01";

/// The AMI, instance type and Availability Zone every launch here names.
const IMAGE_ID: &str = "ami-0abcdef1234567890";
const INSTANCE_TYPE: &str = "t3.micro";
const ZONE: &str = "us-east-1a";

/// The name of the group every VPC comes with, which is what a launch that declares no group
/// of its own is given.
const DEFAULT_GROUP_NAME: &str = "default";

/// How long a leg waits for the instance its group reports to resolve in Amazon EC2, and how
/// often it looks. The deadline is generous and only here so a failure is a failure rather
/// than a hang: the test waits for the CONDITION, never for a fixed time.
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
        "setup: EC2 {action} must succeed, got {status} with body {body}"
    );
    body
}

/// One Auto Scaling action that must succeed.
async fn asg(router: &MultiServiceRouter, action: &str, params: &[(String, String)]) -> String {
    let (status, body) = send(router, "autoscaling", form(action, ASG_VERSION, params)).await;
    assert_eq!(
        status, 200,
        "setup: Auto Scaling {action} must succeed, got {status} with body {body}"
    );
    body
}

/// The network this suite's groups launch into: one VPC, one subnet of it, two security
/// groups of the suite's own and the group the VPC came with.
struct Network {
    subnet: String,
    /// Two groups of the VPC, by id, and the NAME the first of them carries.
    first: String,
    second: String,
    first_name: String,
    /// The group Amazon EC2 itself reports as this VPC's default one.
    default_group: String,
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
                p("CidrBlock", "10.0.1.0/24"),
                p("AvailabilityZone", ZONE),
            ],
        )
        .await,
        "subnetId",
    );
    let first_name = "asg-groups-first".to_string();
    let first = element(
        &ec2(
            router,
            "CreateSecurityGroup",
            &[
                p("GroupName", first_name.clone()),
                p("GroupDescription", "a group a launch template names"),
                p("VpcId", vpc.clone()),
            ],
        )
        .await,
        "groupId",
    );
    let second = element(
        &ec2(
            router,
            "CreateSecurityGroup",
            &[
                p("GroupName", "asg-groups-second"),
                p("GroupDescription", "a second group of the same VPC"),
                p("VpcId", vpc.clone()),
            ],
        )
        .await,
        "groupId",
    );
    // What Amazon EC2 itself calls the default group of this VPC, read through its own
    // filters rather than composed here.
    let default_group = element(
        &ec2(
            router,
            "DescribeSecurityGroups",
            &[
                p("Filter.1.Name", "vpc-id"),
                p("Filter.1.Value.1", vpc.clone()),
                p("Filter.2.Name", "group-name"),
                p("Filter.2.Value.1", DEFAULT_GROUP_NAME),
            ],
        )
        .await,
        "groupId",
    );
    Network {
        subnet,
        first,
        second,
        first_name,
        default_group,
    }
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
    ];
    params.extend_from_slice(version_data);
    element(
        &ec2(router, "CreateLaunchTemplate", &params).await,
        "launchTemplateId",
    )
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

/// Create a group of one instance launching from `launch_template_id` into `subnet`, and
/// answer the id of the instance it launched.
async fn launched_instance(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    subnet: &str,
    launch_template_id: &str,
    source: Source,
) -> String {
    let params = vec![
        p("AutoScalingGroupName", name),
        p("MinSize", "1"),
        p("MaxSize", "1"),
        p("DesiredCapacity", "1"),
        p(source.member(), launch_template_id),
        p("VPCZoneIdentifier", subnet),
    ];
    asg(router, "CreateAutoScalingGroup", &params).await;

    let body = asg(
        router,
        "DescribeAutoScalingGroups",
        &[p("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    let instances = elements(&body, "InstanceId");
    assert_eq!(
        instances.len(),
        1,
        "case {case:?}: a group of one desired instance reports one instance, got \
         {instances:?} in {body}"
    );
    instances
        .into_iter()
        .next()
        .expect("the group reports its one instance")
}

/// The security group ids Amazon EC2 reports for `instance_id`, polled for until the
/// instance resolves.
///
/// Every `groupId` the instance's own report carries: the groups of the instance
/// (`Instance$SecurityGroups`) and the groups of each interface attached to it
/// (`InstanceNetworkInterface$Groups`), as a SET — the two are one fact, so a launch that
/// applied the declared groups to one and a default to the other is a failure here rather
/// than a read that looks at only half of it.
async fn instance_group_ids(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> BTreeSet<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, body) = send(
            router,
            "ec2",
            form(
                "DescribeInstances",
                EC2_API_VERSION,
                &[p("InstanceId.1", instance_id)],
            ),
        )
        .await;
        if status == 200 && body.contains(instance_id) {
            return elements(&body, "groupId").into_iter().collect();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: Amazon EC2 never resolved {instance_id}, which the group reports \
             as its instance: last answer {status}: {body}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Where one row's launch-template version declares its security groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Declares {
    /// The `Groups` of the primary network interface the version declares.
    OnThePrimaryInterface,
    /// The same, with TWO groups on that one interface.
    TwoOnThePrimaryInterface,
    /// A secondary interface listed FIRST with a group of its own, the primary one after it:
    /// the instance's groups are the primary entry's, whatever order the list is in.
    OnThePrimaryInterfaceListedSecond,
    /// The version's own `SecurityGroupIds`.
    OnTheVersionsOwnIds,
    /// The version's own `SecurityGroups`, which names a group by NAME.
    OnTheVersionsOwnNames,
    /// Nowhere: the version declares no group at all.
    Nowhere,
}

impl Declares {
    /// The `LaunchTemplateData` parameters this declaration is carried by. The id list of an
    /// interface is flattened under its member's own `xmlName` (`SecurityGroupId`), the
    /// version's own id list under the same name and its name list under `SecurityGroup`.
    fn version_data(self, network: &Network) -> Vec<(String, String)> {
        let interface = |index: usize, device_index: &str, groups: &[&String]| {
            let mut params = vec![data(
                &format!("NetworkInterface.{index}.DeviceIndex"),
                device_index,
            )];
            for (position, group) in groups.iter().enumerate() {
                params.push(data(
                    &format!("NetworkInterface.{index}.SecurityGroupId.{}", position + 1),
                    (*group).clone(),
                ));
            }
            params
        };
        match self {
            Self::OnThePrimaryInterface => interface(1, "0", &[&network.first]),
            Self::TwoOnThePrimaryInterface => {
                interface(1, "0", &[&network.first, &network.second])
            }
            Self::OnThePrimaryInterfaceListedSecond => {
                let mut params = interface(1, "1", &[&network.second]);
                params.extend(interface(2, "0", &[&network.first]));
                params
            }
            Self::OnTheVersionsOwnIds => vec![data("SecurityGroupId.1", network.first.clone())],
            Self::OnTheVersionsOwnNames => {
                vec![data("SecurityGroup.1", network.first_name.clone())]
            }
            Self::Nowhere => Vec::new(),
        }
    }

    /// The group ids the instance must come up in for this declaration.
    fn expected(self, network: &Network) -> BTreeSet<String> {
        let set = |ids: &[&String]| -> BTreeSet<String> {
            ids.iter().map(|id| (*id).clone()).collect()
        };
        match self {
            Self::OnThePrimaryInterface
            | Self::OnThePrimaryInterfaceListedSecond
            | Self::OnTheVersionsOwnIds
            | Self::OnTheVersionsOwnNames => set(&[&network.first]),
            Self::TwoOnThePrimaryInterface => set(&[&network.first, &network.second]),
            Self::Nowhere => set(&[&network.default_group]),
        }
    }
}

/// One case: where the version declares its groups, and where the group names the version.
struct Row {
    case: &'static str,
    declares: Declares,
    source: Source,
}

const ROWS: &[Row] = &[
    Row {
        case: "one group on the version's primary network interface",
        declares: Declares::OnThePrimaryInterface,
        source: Source::Own,
    },
    Row {
        case: "two groups on that one interface",
        declares: Declares::TwoOnThePrimaryInterface,
        source: Source::Own,
    },
    Row {
        case: "the primary interface listed after a secondary one, each with its own group",
        declares: Declares::OnThePrimaryInterfaceListedSecond,
        source: Source::Own,
    },
    Row {
        case: "the version's own security group ids",
        declares: Declares::OnTheVersionsOwnIds,
        source: Source::Own,
    },
    Row {
        case: "the version's own security group NAMES",
        declares: Declares::OnTheVersionsOwnNames,
        source: Source::Own,
    },
    Row {
        case: "a version that declares no security group at all",
        declares: Declares::Nowhere,
        source: Source::Own,
    },
    Row {
        case: "interface groups, through a mixed instances policy's launch template",
        declares: Declares::OnThePrimaryInterface,
        source: Source::MixedPolicy,
    },
];

#[tokio::test]
async fn an_instance_a_group_launched_comes_up_in_the_groups_its_template_declares() {
    let router = router();
    let network = seed_network(&router).await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let template = create_template(
            &router,
            &format!("asg-lt-groups-{index}"),
            &row.declares.version_data(&network),
        )
        .await;
        let instance = launched_instance(
            &router,
            case,
            &format!("asg-groups-{index}"),
            &network.subnet,
            &template,
            row.source,
        )
        .await;
        let read = instance_group_ids(&router, case, &instance).await;
        let wanted = row.declares.expected(&network);
        assert_eq!(
            read, wanted,
            "case {case:?}: expected the instance {instance} to come up in {wanted:?}, got \
             {read:?}"
        );
        // The declared groups are the instance's INSTEAD of the VPC's default one, which is
        // the difference the rule makes: a launch that read nothing would report that group.
        if row.declares != Declares::Nowhere {
            assert!(
                !read.contains(&network.default_group),
                "case {case:?}: the VPC's default group {} must not be applied to an \
                 instance whose version declares its own, got {read:?}",
                network.default_group
            );
        }
    }
}
