// SPDX-License-Identifier: CC0-1.0

//! The SECURITY GROUPS an instance an EC2 Fleet placed comes up in, driven over the wire the
//! service actually speaks — `ec2Query`, through the crate's shared [`super::ec2_query`]
//! harness.
//!
//! RULE: a launch names the security groups of its instance in ONE of two places, never both
//! — the launch-template version's own `SecurityGroupIds` / `SecurityGroups`, or the `Groups`
//! of the network interfaces the version declares, since "if you specify a network interface,
//! you must specify any security groups as part of the network interface instead of using
//! this parameter"
//! (`smithy_query(query="doc",shape="RequestLaunchTemplateData",member="SecurityGroupIds")`)
//! — and an instance's groups are the ones applied to its PRIMARY network interface, the
//! entry at device index 0 ("the primary network interface has a device index of 0",
//! `smithy_query(query="doc",shape="LaunchTemplateInstanceNetworkInterfaceSpecificationRequest",member="DeviceIndex")`),
//! which is the index an entry that names none takes. A fleet places its capacity from a
//! launch-template version, so an instance it placed carries the groups that version
//! declares; only a version that declares none leaves its instances in the VPC's default
//! group ("Default: Amazon Web Services uses the default security group",
//! `RunInstancesRequest$SecurityGroupIds`).
//!
//! The rows span what the rule generalises over: WHERE the version declares its groups (on
//! the primary interface, on its own id list, on its own NAME list, nowhere), HOW MANY it
//! declares there (one, two) and the ORDER its interfaces are listed in (the primary after a
//! secondary one, each with a group of its own).
//!
//! Every id asserted is one this service minted — the groups through `CreateSecurityGroup`,
//! the VPC's default group through the service's own filters — and every instance is read
//! back through `DescribeInstances`, where a client reads an instance's groups.

use std::collections::BTreeSet;

use ec2::state::ComAmazonawsEc2Amazonec2State as Ec2State;
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, emulator};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The instance type every launch template here names.
const INSTANCE_TYPE: &str = "t3.micro";

/// An AMI of the published catalog a fresh account can already see, read out of the
/// service's OWN state rather than written here: a launch template a fleet places from must
/// name an image the account can resolve, so the suite cannot invent an id.
fn image_id() -> String {
    Ec2State::default()
        .images
        .keys()
        .next()
        .cloned()
        .expect("a fresh account must publish at least one image")
}

/// The name of the group every VPC comes with, which is what a launch that declares no group
/// of its own is given.
const DEFAULT_GROUP_NAME: &str = "default";

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// One `LaunchTemplateData.<member>` parameter of a launch-template create.
fn data(member: &str, value: impl Into<String>) -> (String, String) {
    p(&format!("LaunchTemplateData.{member}"), value)
}

/// One action that must succeed, answering its body.
async fn ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(String, String)],
) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {action} must succeed, got {status}: {body}"
    );
    body
}

/// The network a fleet of this suite places into: one VPC, one subnet of it in the Region's
/// first zone, two groups of the suite's own and the group the VPC came with.
struct Network {
    subnet: String,
    zone: String,
    first: String,
    second: String,
    first_name: String,
    default_group: String,
}

async fn seed_network(router: &MultiServiceRouter) -> Network {
    let case = "the suite's own network";
    let zone = format!("{REGION}a");
    let vpc = element(
        &ok(router, case, "CreateVpc", &[p("CidrBlock", "10.9.0.0/16")]).await,
        "vpcId",
    )
    .expect("CreateVpc reports the VPC it created");
    let subnet = element(
        &ok(
            router,
            case,
            "CreateSubnet",
            &[
                p("VpcId", vpc.clone()),
                p("CidrBlock", "10.9.1.0/24"),
                p("AvailabilityZone", zone.clone()),
            ],
        )
        .await,
        "subnetId",
    )
    .expect("CreateSubnet reports the subnet it created");
    let first_name = "fleet-groups-first".to_string();
    let group = |name: &str, description: &str| {
        vec![
            p("GroupName", name),
            p("GroupDescription", description),
            p("VpcId", vpc.clone()),
        ]
    };
    let first = element(
        &ok(
            router,
            case,
            "CreateSecurityGroup",
            &group(&first_name, "a group a launch template names"),
        )
        .await,
        "groupId",
    )
    .expect("CreateSecurityGroup reports the group it created");
    let second = element(
        &ok(
            router,
            case,
            "CreateSecurityGroup",
            &group("fleet-groups-second", "a second group of the same VPC"),
        )
        .await,
        "groupId",
    )
    .expect("CreateSecurityGroup reports the group it created");
    // What the service itself calls the default group of this VPC, read through its own
    // filters rather than composed here.
    let default_group = element(
        &ok(
            router,
            case,
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
    )
    .expect("a VPC comes with its default security group");
    Network {
        subnet,
        zone,
        first,
        second,
        first_name,
        default_group,
    }
}

/// Where one row's launch-template version declares its security groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Declares {
    /// The `Groups` of the primary network interface the version declares.
    OnThePrimaryInterface,
    /// The same, with TWO groups on that one interface.
    TwoOnThePrimaryInterface,
    /// A secondary interface listed FIRST with a group of its own, the primary one after it.
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

    /// The group ids the placed instance must come up in for this declaration.
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

/// One case: where the version the fleet places from declares its groups.
struct Row {
    case: &'static str,
    declares: Declares,
}

const ROWS: &[Row] = &[
    Row {
        case: "one group on the version's primary network interface",
        declares: Declares::OnThePrimaryInterface,
    },
    Row {
        case: "two groups on that one interface",
        declares: Declares::TwoOnThePrimaryInterface,
    },
    Row {
        case: "the primary interface listed after a secondary one, each with its own group",
        declares: Declares::OnThePrimaryInterfaceListedSecond,
    },
    Row {
        case: "the version's own security group ids",
        declares: Declares::OnTheVersionsOwnIds,
    },
    Row {
        case: "the version's own security group NAMES",
        declares: Declares::OnTheVersionsOwnNames,
    },
    Row {
        case: "a version that declares no security group at all",
        declares: Declares::Nowhere,
    },
];

/// The security group ids `DescribeInstances` reports for one instance: the groups of the
/// instance (`Instance$SecurityGroups`) and of each interface attached to it
/// (`InstanceNetworkInterface$Groups`), as a SET — the two are one fact, so a launch that
/// applied the declared groups to one and a default to the other fails here rather than
/// passing a read that looks at only half of it.
async fn reported_group_ids(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> BTreeSet<String> {
    let body = ok(
        router,
        case,
        "DescribeInstances",
        &[p("InstanceId.1", instance_id)],
    )
    .await;
    assert!(
        body.contains(instance_id),
        "case {case:?}: DescribeInstances must report {instance_id}, got {body}"
    );
    elements(&body, "groupId").into_iter().collect()
}

#[tokio::test]
async fn an_instance_a_fleet_placed_comes_up_in_the_groups_its_template_declares() {
    let (router, _engine) = emulator();
    let network = seed_network(&router).await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let mut template = vec![
            p("LaunchTemplateName", format!("fleet-lt-groups-{index}")),
            data("ImageId", image_id()),
            data("InstanceType", INSTANCE_TYPE),
        ];
        template.extend(row.declares.version_data(&network));
        let template_id = element(
            &ok(&router, case, "CreateLaunchTemplate", &template).await,
            "launchTemplateId",
        )
        .unwrap_or_else(|| panic!("case {case:?}: CreateLaunchTemplate reports the template"));

        // An `instant` fleet places its capacity before it answers and reports the instances
        // it launched, so the instance under test is the one this call names.
        let body = ok(
            &router,
            case,
            "CreateFleet",
            &[
                p(
                    "LaunchTemplateConfigs.1.LaunchTemplateSpecification.LaunchTemplateId",
                    template_id,
                ),
                p(
                    "LaunchTemplateConfigs.1.Overrides.1.SubnetId",
                    network.subnet.clone(),
                ),
                p(
                    "LaunchTemplateConfigs.1.Overrides.1.AvailabilityZone",
                    network.zone.clone(),
                ),
                p("Type", "instant"),
                p("TargetCapacitySpecification.TotalTargetCapacity", "1"),
                p("TargetCapacitySpecification.OnDemandTargetCapacity", "1"),
            ],
        )
        .await;
        // "Information about the instances that were launched by the fleet" is a SET, so the
        // ids are the `<item>` texts of the `<instanceIds>` element rather than elements of
        // their own.
        let placed = element(&body, "instanceIds").unwrap_or_default();
        let instance_id = elements(&placed, "item")
            .into_iter()
            .next()
            .unwrap_or_else(|| {
                panic!("case {case:?}: the instant fleet reports what it placed, got {body}")
            });

        let read = reported_group_ids(&router, case, &instance_id).await;
        let wanted = row.declares.expected(&network);
        assert_eq!(
            read, wanted,
            "case {case:?}: expected the instance {instance_id} to come up in {wanted:?}, got \
             {read:?}"
        );
        // The declared groups are the instance's INSTEAD of the VPC's default one, which is
        // the difference the rule makes: a placement that read nothing would report that
        // group.
        if row.declares != Declares::Nowhere {
            assert!(
                !read.contains(&network.default_group),
                "case {case:?}: the VPC's default group {} must not be applied to an instance \
                 whose version declares its own, got {read:?}",
                network.default_group
            );
        }
    }
}
