// SPDX-License-Identifier: CC0-1.0

//! The REGISTRATION, in Amazon EC2 Auto Scaling, of every Auto Scaling group a deployed stack
//! declares — whichever spelling the declaration uses for the group's name, its placement and
//! its launch source.
//!
//! RULE: a stack that has reached a terminal success has created, in Amazon EC2 Auto Scaling,
//! one group per `AWS::AutoScaling::AutoScalingGroup` its template declares, registered under
//! the name the stack reports as that resource's `PhysicalResourceId`, so
//! `DescribeAutoScalingGroups` filtered to that name answers with exactly that one group
//! carrying the launch source, the sizes and the instances the declaration asked for — and a
//! name no deployment declared matches no group at all.
//!
//! Why it matters: the filtered read is the FIRST call a workflow makes with the name a
//! completed deployment just handed it. A group the deployment reports created but that read
//! answers an empty list for tells an operator a successful deployment produced nothing, and
//! every later step — the group's capacity, its instance list, the network interfaces of those
//! instances — then operates on nothing.
//!
//! The rows differ along what the rule generalises over, which is the FORM of the declaration
//! rather than any one template: whether the group's name is the declaration's own or the one
//! CloudFormation decides; whether its placement is a LIST of subnets, a single subnet spelled
//! without the sequence syntax, or Availability Zones with no subnet at all; whether its launch
//! source names the launch template by the id Amazon EC2 minted or by the template's name; and
//! whether the declaration states a `DesiredCapacity` at all — a declaration that states none
//! runs at the group's minimum size, and one that states the maximum runs there.
//!
//! Every row deploys its own stack onto ONE emulator, so the filtered read has several
//! registered groups to pick from: that a read for one name answers with one group is only
//! meaningful where more than one group exists.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::auto_scaling_group_reads::{describe_groups, described_group, in_service_instances};
use crate::stack_lifecycle::{
    call, create_stack, is_service_assigned_id, member, router_with_engine, settled_stack,
    xml_blocks, xml_text, xml_texts,
};

/// The AMI the launch template's data names, in Amazon EC2's own id form (`ami-` plus 17
/// hexadecimal characters).
const IMAGE_ID: &str = "ami-00000000000000001";

/// The two Availability Zones of the Region these stacks deploy in that the estate uses: the
/// subnets are created in them, and the row that places its group by ZONE names them.
const ZONE_A: &str = "us-east-1a";
const ZONE_B: &str = "us-east-1b";

/// A group name no row's deployment declares, for the read that must match nothing.
const ABSENT_GROUP: &str = "asg-registration-no-such-group";

/// How a declaration states where the group places its instances.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placement {
    /// `VPCZoneIdentifier` as a LIST of two subnet references.
    SubnetList,
    /// `VPCZoneIdentifier` as ONE subnet reference, spelled without the sequence syntax.
    OneSubnet,
    /// `AvailabilityZones` alone, naming no subnet.
    Zones,
}

/// How a declaration states the launch template the group launches from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaunchSource {
    /// By the `lt-` id Amazon EC2 minted for the template, which is what `Ref` of it returns.
    ById,
    /// By the template's NAME.
    ByName,
}

/// One declared group: the stack it is deployed as, and the form its declaration takes.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The name the declaration states for the group, or `None` where CloudFormation decides
    /// it.
    declared_group_name: Option<&'static str>,
    placement: Placement,
    launch_source: LaunchSource,
    min_size: u32,
    max_size: u32,
    /// The `DesiredCapacity` the declaration states, or `None` where it states none.
    declared_desired: Option<u32>,
    /// The capacity the group must then run at: the declared one, or the group's minimum size
    /// where the declaration states none ("if you do not specify a desired capacity, the
    /// default is the minimum size of the group", `CreateAutoScalingGroup$DesiredCapacity`).
    capacity: u32,
}

/// Every form of declaration the rule covers, one row each.
const ROWS: &[Row] = &[
    Row {
        case: "CloudFormation decides the name; a list of subnets; the template by id; a \
               declared desired capacity",
        stack: "asg-registration-unnamed",
        declared_group_name: None,
        placement: Placement::SubnetList,
        launch_source: LaunchSource::ById,
        min_size: 1,
        max_size: 4,
        declared_desired: Some(2),
        capacity: 2,
    },
    Row {
        case: "the declaration names the group; Availability Zones with no subnet; the \
               template by name; no desired capacity declared",
        stack: "asg-registration-zoned",
        declared_group_name: Some("asg-registration-zoned-group"),
        placement: Placement::Zones,
        launch_source: LaunchSource::ByName,
        min_size: 2,
        max_size: 5,
        declared_desired: None,
        capacity: 2,
    },
    Row {
        case: "the declaration names the group; one subnet spelled without the sequence \
               syntax; the template by id; the desired capacity at the maximum size",
        stack: "asg-registration-single-subnet",
        declared_group_name: Some("asg-registration-single-subnet-group"),
        placement: Placement::OneSubnet,
        launch_source: LaunchSource::ById,
        min_size: 1,
        max_size: 3,
        declared_desired: Some(3),
        capacity: 3,
    },
];

/// The name the row's template gives its launch template, which is what the row that names
/// its launch source by NAME states in the group's `LaunchTemplate`.
fn launch_template_name(row: &Row) -> String {
    format!("{}-lt", row.stack)
}

/// The row's template: a VPC with a subnet in each of the two zones, a launch template, and
/// the one group whose declaration this row is about.
fn template(row: &Row) -> String {
    let lt_name = launch_template_name(row);
    let mut group = json!({
        "MinSize": row.min_size.to_string(),
        "MaxSize": row.max_size.to_string(),
    });
    if let Some(name) = row.declared_group_name {
        group["AutoScalingGroupName"] = json!(name);
    }
    if let Some(desired) = row.declared_desired {
        group["DesiredCapacity"] = json!(desired.to_string());
    }
    // The launch source. Both forms carry the template's LATEST version number, which is an
    // attribute only Amazon EC2 can answer — so the group is declared after the template it
    // launches from whichever way it names it.
    let version = json!({"Fn::GetAtt": ["Lt", "LatestVersionNumber"]});
    group["LaunchTemplate"] = match row.launch_source {
        LaunchSource::ById => json!({"LaunchTemplateId": {"Ref": "Lt"}, "Version": version}),
        LaunchSource::ByName => json!({"LaunchTemplateName": lt_name, "Version": version}),
    };
    match row.placement {
        Placement::SubnetList => {
            group["VPCZoneIdentifier"] = json!([{"Ref": "SubnetA"}, {"Ref": "SubnetB"}]);
        }
        Placement::OneSubnet => group["VPCZoneIdentifier"] = json!({"Ref": "SubnetA"}),
        Placement::Zones => group["AvailabilityZones"] = json!([ZONE_A, ZONE_B]),
    }
    let declared: Value = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Vpc": {
                "Type": "AWS::EC2::VPC",
                "Properties": {"CidrBlock": "10.174.0.0/16"},
            },
            "SubnetA": {
                "Type": "AWS::EC2::Subnet",
                "Properties": {
                    "VpcId": {"Ref": "Vpc"},
                    "CidrBlock": "10.174.1.0/24",
                    "AvailabilityZone": ZONE_A,
                },
            },
            "SubnetB": {
                "Type": "AWS::EC2::Subnet",
                "Properties": {
                    "VpcId": {"Ref": "Vpc"},
                    "CidrBlock": "10.174.2.0/24",
                    "AvailabilityZone": ZONE_B,
                },
            },
            "Lt": {
                "Type": "AWS::EC2::LaunchTemplate",
                "Properties": {
                    "LaunchTemplateName": lt_name,
                    "LaunchTemplateData": {
                        "ImageId": IMAGE_ID,
                        "InstanceType": "t3.micro",
                    },
                },
            },
            "Asg": {
                "Type": "AWS::AutoScaling::AutoScalingGroup",
                "Properties": group,
            },
        },
    });
    declared.to_string()
}

/// The `PhysicalResourceId` a stack reports for one of its logical resources, read the way a
/// client that has just deployed reads it.
async fn physical_id(
    case: &str,
    router: &MultiServiceRouter,
    stack: &str,
    logical_id: &str,
) -> String {
    let (status, body) = call(
        router,
        "DescribeStackResources",
        &[
            member("StackName", stack),
            member("LogicalResourceId", logical_id),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeStackResources({logical_id}) must answer 200, got {status} \
         {body}"
    );
    xml_text(&body, "PhysicalResourceId").unwrap_or_else(|| {
        panic!("case {case:?}: {logical_id} must report a PhysicalResourceId, got {body}")
    })
}

/// The zones the group reports, sorted, so a case asserts WHICH zones rather than the order
/// one resolution happened to produce.
fn reported_zones(group: &str) -> Vec<String> {
    let mut zones: Vec<String> = xml_blocks(group, "AvailabilityZones")
        .iter()
        .flat_map(|block| xml_texts(block, "member"))
        .collect();
    zones.sort();
    zones
}

/// One group's scalar member, read off its own described body.
fn reported(case: &str, group: &str, tag: &str) -> String {
    xml_text(group, tag)
        .unwrap_or_else(|| panic!("case {case:?}: the group must report its {tag}, got {group}"))
}

#[tokio::test]
async fn every_declared_group_is_registered_under_the_name_the_deployment_reports() {
    let (router, _engine) = router_with_engine();

    // Every row's stack is created first and settled afterwards: the groups are registered in
    // ONE emulator, which is what makes a filtered read discriminating.
    for row in ROWS {
        let (status, body) = create_stack(&router, row.stack, &template(row), &[]).await;
        assert_eq!(
            status, 200,
            "case {:?}: CreateStack must answer 200, got {status} {body}",
            row.case
        );
    }

    let mut registered: Vec<String> = Vec::new();
    for row in ROWS {
        let case = row.case;
        let described = settled_stack(&router, case, row.stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the stack to deploy, got {described}"
        );

        // The name the deployment reports for the group: the declaration's own where it states
        // one, and otherwise one CloudFormation decided — never the bare logical id.
        let group_name = physical_id(case, &router, row.stack, "Asg").await;
        match row.declared_group_name {
            Some(declared) => assert_eq!(
                group_name, declared,
                "case {case:?}: expected the declared group name {declared:?}, got \
                 {group_name:?}"
            ),
            // A resource the declaration leaves unnamed is reported under a name
            // CloudFormation decided, which is NOT the bare logical id; WHICH name it decided
            // is not fixed by anything the template states, so what pins it is the owning
            // service answering for that exact value, asserted just below.
            None => assert!(
                !group_name.is_empty() && group_name != "Asg",
                "case {case:?}: expected a name CloudFormation generated for the unnamed \
                 group, got {group_name:?}"
            ),
        }
        registered.push(group_name.clone());

        // ...and that one call, with that one value, reaches the group: no wait, no second
        // read, exactly as a workflow makes it.
        let group = described_group(case, &router, &group_name).await;

        // The sizes the declaration asked for, including the desired capacity a declaration
        // that states none runs at.
        assert_eq!(
            (
                reported(case, &group, "MinSize"),
                reported(case, &group, "MaxSize"),
                reported(case, &group, "DesiredCapacity"),
            ),
            (
                row.min_size.to_string(),
                row.max_size.to_string(),
                row.capacity.to_string(),
            ),
            "case {case:?}: expected the group to run at the declared sizes {}/{}/{}, got \
             {group}",
            row.min_size,
            row.max_size,
            row.capacity
        );

        // The ARN is the owning service's own, built on the name the group is registered
        // under.
        let arn = reported(case, &group, "AutoScalingGroupARN");
        assert!(
            arn.contains(":autoscaling:") && arn.ends_with(&group_name),
            "case {case:?}: expected the ARN Amazon EC2 Auto Scaling builds for \
             {group_name:?}, got {arn:?}"
        );

        // The launch source, in BOTH spellings: the group resolves the template the
        // declaration named, so a read reports the id Amazon EC2 minted and the template's
        // name whichever of the two the declaration stated.
        let launch_template = physical_id(case, &router, row.stack, "Lt").await;
        assert!(
            is_service_assigned_id(&launch_template, "lt"),
            "case {case:?}: expected the launch template id Amazon EC2 mints, got \
             {launch_template:?}"
        );
        assert_eq!(
            (
                reported(case, &group, "LaunchTemplateId"),
                reported(case, &group, "LaunchTemplateName"),
            ),
            (launch_template.clone(), launch_template_name(row)),
            "case {case:?}: expected the group to report the launch template it launches \
             from, got {group}"
        );

        // The placement, in the spelling the declaration used and in the zones that spelling
        // resolves to: a group that names only subnets still has zones, which are the zones
        // of those subnets.
        let subnet_a = physical_id(case, &router, row.stack, "SubnetA").await;
        let subnet_b = physical_id(case, &router, row.stack, "SubnetB").await;
        let subnets = xml_text(&group, "VPCZoneIdentifier").unwrap_or_default();
        match row.placement {
            Placement::SubnetList => {
                assert_eq!(
                    subnets,
                    format!("{subnet_a},{subnet_b}"),
                    "case {case:?}: expected the declared subnets, got {subnets:?}"
                );
                assert_eq!(
                    reported_zones(&group),
                    vec![ZONE_A.to_string(), ZONE_B.to_string()],
                    "case {case:?}: expected the zones of the declared subnets, got {group}"
                );
            }
            Placement::OneSubnet => {
                assert_eq!(
                    subnets, subnet_a,
                    "case {case:?}: expected the one declared subnet, got {subnets:?}"
                );
                assert_eq!(
                    reported_zones(&group),
                    vec![ZONE_A.to_string()],
                    "case {case:?}: expected the zone of the one declared subnet, got {group}"
                );
            }
            Placement::Zones => {
                assert!(
                    subnets.is_empty(),
                    "case {case:?}: a group declared by zone names no subnet, got {subnets:?}"
                );
                assert_eq!(
                    reported_zones(&group),
                    vec![ZONE_A.to_string(), ZONE_B.to_string()],
                    "case {case:?}: expected the declared zones, got {group}"
                );
            }
        }

        // ...and the group really holds what it was sized for, which is what makes the
        // stack's completion a group a client can drive.
        let instances = in_service_instances(case, &router, &group_name, row.capacity as usize)
            .await;
        for instance_id in &instances {
            assert!(
                is_service_assigned_id(instance_id, "i"),
                "case {case:?}: expected the instance ids Amazon EC2 assigned, got \
                 {instances:?}"
            );
        }
    }

    // The filter DISCRIMINATES: a name no deployment declared matches no group, and a read
    // naming every deployed group reports all of them and nothing else.
    let absent = "a name no deployment declared";
    let (body, names) = describe_groups(absent, &router, &[ABSENT_GROUP]).await;
    assert!(
        names.is_empty(),
        "case {absent:?}: expected no group for {ABSENT_GROUP:?}, got {names:?} in {body}"
    );

    let mut expected = registered.clone();
    expected.sort();
    let every = "a read naming every deployed group";
    let filter: Vec<&str> = registered.iter().map(String::as_str).collect();
    let (body, mut names) = describe_groups(every, &router, &filter).await;
    names.sort();
    assert_eq!(
        names, expected,
        "case {every:?}: expected exactly the deployed groups, got {body}"
    );

    // ...and the unfiltered read reports exactly the groups the deployments created, so no
    // group was registered under a second name beside the one the stack reports.
    let all = "the unfiltered read";
    let (body, mut names) = describe_groups(all, &router, &[]).await;
    names.sort();
    assert_eq!(
        names, expected,
        "case {all:?}: expected exactly the deployed groups, got {body}"
    );
}
