// SPDX-License-Identifier: CC0-1.0

//! The HAND-OFF from a deployed stack to the service that owns what it declared, for the
//! fleet shape a client deploys an Auto Scaling group as.
//!
//! RULE: a stack that has reached a terminal success reports, for EVERY output its template
//! declares, an `OutputValue` with the intrinsic that produced it fully evaluated, and the
//! `PhysicalResourceId` it reports for a resource whose type an owning service holds is the
//! identifier THAT service answers its own read with — so the one call a client makes next,
//! with the value it just read, reaches the resource the stack says it created.
//!
//! Why it matters: those two members are the whole of what a deployment hands its caller. An
//! output reported with a key and no value cannot be consumed at all (the standard
//! `{o["OutputKey"]: o["OutputValue"] …}` read raises on it), and a physical id the owning
//! service does not answer for turns the next call into a not-found in a neighbouring
//! service, far from the stack that minted it.
//!
//! The rows differ along what the rule generalises over: the intrinsic FORM the output's
//! value is written as (`Ref`, `Fn::GetAtt`, `Fn::Sub` over each of those two, `Fn::Join`
//! over a list of references), the resource it reads (the group, whose name CloudFormation
//! decides; the launch template, whose id Amazon EC2 mints; the subnets), and whether the
//! output also declares a `Description` and an `Export`. The same reads are taken again
//! after an in-place UPDATE of the group, since a client deploys more than once.
//!
//! The estate is the one the deployment of an Auto Scaling group takes: a VPC with two
//! subnets, a launch template, and a group that launches into those subnets at a desired
//! capacity of two — and the group's instances are read back from Auto Scaling itself, so a
//! group reported created is one that really holds what it was sized for.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::auto_scaling_group_reads::{described_group, in_service_instances};
use crate::stack_lifecycle::{
    call, create_stack, is_service_assigned_id, member, router_with_engine, xml_text,
};
use crate::stack_outputs::wire_outputs;

/// The stack the estate is deployed as.
const STACK: &str = "asg-handoff";

/// The AMI the launch template's data names, in Amazon EC2's own id form (`ami-` plus 17
/// hexadecimal characters).
const IMAGE_ID: &str = "ami-00000000000000001";

/// The capacity the group is declared at, and the one a later update moves it to. Both are
/// inside the declared `MinSize`/`MaxSize`, so neither is a change the owning service would
/// refuse for being outside the group's own bounds.
const CAPACITY: usize = 2;
const UPDATED_CAPACITY: usize = 3;

/// What one output's value must BE, judged against what the owning services answer their own
/// reads with rather than against a literal this suite made up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// Exactly the group's own name — the physical id the stack reports for it.
    GroupName,
    /// Exactly the ARN Amazon EC2 Auto Scaling reports for that group.
    GroupArn,
    /// The group's name, inside a value the template's `Fn::Sub` wrote around it.
    GroupNameAfter(&'static str),
    /// The group's ARN, inside such a value.
    GroupArnAfter(&'static str),
    /// Exactly the launch template id Amazon EC2 minted.
    LaunchTemplateId,
    /// The launch template's latest version number, as that service keeps it.
    LaunchTemplateVersion,
    /// The two subnet ids Amazon EC2 assigned, joined as the template joins them.
    JoinedSubnetIds,
}

/// One declared output: the key a client reads it under, the intrinsic form its value is
/// written as, and what that value must be.
struct Row {
    case: &'static str,
    key: &'static str,
    expected: Expected,
}

/// Every output the template declares, one row each.
const ROWS: &[Row] = &[
    Row {
        case: "Ref of the group, whose name CloudFormation decides",
        key: "GroupName",
        expected: Expected::GroupName,
    },
    Row {
        case: "Fn::GetAtt of the group's Arn, which only Auto Scaling can answer",
        key: "GroupArn",
        expected: Expected::GroupArn,
    },
    Row {
        case: "Fn::Sub over a Ref of the group",
        key: "SubbedGroupName",
        expected: Expected::GroupNameAfter("group="),
    },
    Row {
        case: "Fn::Sub over an Fn::GetAtt of the group's Arn",
        key: "SubbedGroupArn",
        expected: Expected::GroupArnAfter("arn="),
    },
    Row {
        case: "Ref of the launch template, whose id Amazon EC2 mints",
        key: "LaunchTemplateId",
        expected: Expected::LaunchTemplateId,
    },
    Row {
        case: "Fn::GetAtt of the launch template's latest version number",
        key: "LaunchTemplateVersion",
        expected: Expected::LaunchTemplateVersion,
    },
    Row {
        case: "Fn::Join over a list of references to the two subnets",
        key: "SubnetIds",
        expected: Expected::JoinedSubnetIds,
    },
    Row {
        case: "an output that also declares a Description and an Export",
        key: "ExportedGroupName",
        expected: Expected::GroupName,
    },
];

/// The `Description` the exported row declares, reported beside its value.
const EXPORTED_DESCRIPTION: &str = "the group this stack deploys";

/// The `Export.Name` the exported row declares.
const EXPORT_NAME: &str = "asg-handoff-group";

/// The estate's template at `capacity`: the network, the launch template, the group that
/// launches into the network's subnets, and one output per [`ROWS`] entry.
fn template(capacity: usize) -> String {
    let declared: Value = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Vpc": {
                "Type": "AWS::EC2::VPC",
                "Properties": {"CidrBlock": "10.173.0.0/16"},
            },
            "SubnetA": {
                "Type": "AWS::EC2::Subnet",
                "Properties": {
                    "VpcId": {"Ref": "Vpc"},
                    "CidrBlock": "10.173.1.0/24",
                    "AvailabilityZone": "us-east-1a",
                },
            },
            "SubnetB": {
                "Type": "AWS::EC2::Subnet",
                "Properties": {
                    "VpcId": {"Ref": "Vpc"},
                    "CidrBlock": "10.173.2.0/24",
                    "AvailabilityZone": "us-east-1b",
                },
            },
            "Lt": {
                "Type": "AWS::EC2::LaunchTemplate",
                "Properties": {
                    "LaunchTemplateData": {
                        "ImageId": IMAGE_ID,
                        "InstanceType": "t3.micro",
                    },
                },
            },
            "Asg": {
                "Type": "AWS::AutoScaling::AutoScalingGroup",
                "Properties": {
                    "MinSize": "1",
                    "MaxSize": "4",
                    "DesiredCapacity": capacity.to_string(),
                    "VPCZoneIdentifier": [{"Ref": "SubnetA"}, {"Ref": "SubnetB"}],
                    "LaunchTemplate": {
                        "LaunchTemplateId": {"Ref": "Lt"},
                        "Version": {"Fn::GetAtt": ["Lt", "LatestVersionNumber"]},
                    },
                },
            },
        },
        "Outputs": {
            "GroupName": {"Value": {"Ref": "Asg"}},
            "GroupArn": {"Value": {"Fn::GetAtt": ["Asg", "Arn"]}},
            "SubbedGroupName": {"Value": {"Fn::Sub": "group=${Asg}"}},
            "SubbedGroupArn": {"Value": {"Fn::Sub": "arn=${Asg.Arn}"}},
            "LaunchTemplateId": {"Value": {"Ref": "Lt"}},
            "LaunchTemplateVersion": {
                "Value": {"Fn::GetAtt": ["Lt", "LatestVersionNumber"]}
            },
            "SubnetIds": {
                "Value": {"Fn::Join": [",", [{"Ref": "SubnetA"}, {"Ref": "SubnetB"}]]}
            },
            "ExportedGroupName": {
                "Value": {"Ref": "Asg"},
                "Description": EXPORTED_DESCRIPTION,
                "Export": {"Name": EXPORT_NAME},
            },
        },
    });
    declared.to_string()
}

/// The `PhysicalResourceId` the stack reports for one of its logical resources, read the way
/// a client that has just deployed reads it.
async fn physical_id(case: &str, router: &MultiServiceRouter, logical_id: &str) -> String {
    let (status, body) = call(
        router,
        "DescribeStackResources",
        &[
            member("StackName", STACK),
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

/// Every output the stack reports, as the key-to-value pairs a client consumes them as —
/// with the value ABSENT where the response carried no `OutputValue`, so an output that
/// carries none is visible as such rather than borrowing its neighbour's.
fn declared_outputs(body: &str) -> Vec<(String, Option<String>)> {
    wire_outputs(body)
        .into_iter()
        .map(|output| (output.key.unwrap_or_default(), output.value))
        .collect()
}

/// One output's `OutputValue`, asserted PRESENT: the half of the rule a client reads the
/// deployment through.
fn value_of(case: &str, outputs: &[(String, Option<String>)], key: &str) -> String {
    let found = outputs
        .iter()
        .find(|(name, _)| name == key)
        .unwrap_or_else(|| panic!("case {case:?}: no output named {key:?}, got {outputs:?}"));
    found.1.clone().unwrap_or_else(|| {
        panic!("case {case:?}: the output {key:?} carries no OutputValue, got {outputs:?}")
    })
}

/// The stack's whole `DescribeStacks` body once it has settled, asserted to have settled on
/// `expected`.
async fn settled_on(case: &str, router: &MultiServiceRouter, expected: &str) -> String {
    let body = crate::stack_lifecycle::settled_stack(router, case, STACK).await;
    assert_eq!(
        xml_text(&body, "StackStatus").as_deref(),
        Some(expected),
        "case {case:?}: expected the stack to settle on {expected}, got {body}"
    );
    body
}

/// Every row's value, judged against what the owning services answer: the group's own read,
/// the launch template id the stack reports for the template, and the subnet ids it reports
/// for the subnets.
async fn assert_every_output(
    router: &MultiServiceRouter,
    phase: &str,
    body: &str,
    group_name: &str,
    group_arn: &str,
) {
    let outputs = declared_outputs(body);
    assert_eq!(
        outputs.len(),
        ROWS.len(),
        "case {phase:?}: expected one reported output per declared output, got {outputs:?}"
    );
    let launch_template = physical_id(phase, router, "Lt").await;
    let subnets = format!(
        "{},{}",
        physical_id(phase, router, "SubnetA").await,
        physical_id(phase, router, "SubnetB").await
    );
    for row in ROWS {
        let case = format!("{phase}: {}", row.case);
        let value = value_of(&case, &outputs, row.key);
        match row.expected {
            Expected::GroupName => assert_eq!(
                value, group_name,
                "case {case:?}: expected the group's own name {group_name:?}, got {value:?}"
            ),
            Expected::GroupArn => assert_eq!(
                value, group_arn,
                "case {case:?}: expected the ARN Auto Scaling reports, {group_arn:?}, got \
                 {value:?}"
            ),
            Expected::GroupNameAfter(prefix) => assert_eq!(
                value,
                format!("{prefix}{group_name}"),
                "case {case:?}: expected {prefix:?} followed by the group's name, got {value:?}"
            ),
            Expected::GroupArnAfter(prefix) => assert_eq!(
                value,
                format!("{prefix}{group_arn}"),
                "case {case:?}: expected {prefix:?} followed by the group's ARN, got {value:?}"
            ),
            Expected::LaunchTemplateId => {
                assert!(
                    is_service_assigned_id(&value, "lt"),
                    "case {case:?}: expected the launch template id Amazon EC2 mints, got \
                     {value:?}"
                );
                assert_eq!(
                    value, launch_template,
                    "case {case:?}: expected the id the stack reports for the template, got \
                     {value:?}"
                );
            }
            Expected::LaunchTemplateVersion => assert!(
                value.parse::<u64>().is_ok_and(|number| number >= 1),
                "case {case:?}: expected the template's version number, got {value:?}"
            ),
            Expected::JoinedSubnetIds => assert_eq!(
                value, subnets,
                "case {case:?}: expected the subnet ids the stack reports, got {value:?}"
            ),
        }
    }
    // The members an output declares BESIDE its value are reported with it: the description
    // and the export name belong to the one row that declares them, and to no other.
    let exported = wire_outputs(body)
        .into_iter()
        .find(|output| output.key.as_deref() == Some("ExportedGroupName"))
        .unwrap_or_else(|| panic!("case {phase:?}: the exported output must be reported"));
    assert_eq!(
        (exported.description.as_deref(), exported.export_name.as_deref()),
        (Some(EXPORTED_DESCRIPTION), Some(EXPORT_NAME)),
        "case {phase:?}: expected the declared Description and ExportName beside the value, \
         got {exported:?}"
    );
}

#[tokio::test]
async fn a_deployed_group_is_readable_through_the_outputs_and_in_auto_scaling() {
    let (router, _engine) = router_with_engine();
    let created = "a created stack declaring an Auto Scaling group";
    let (status, body) = create_stack(&router, STACK, &template(CAPACITY), &[]).await;
    assert_eq!(
        status, 200,
        "case {created:?}: CreateStack must answer 200, got {status} {body}"
    );
    let described = settled_on(created, &router, "CREATE_COMPLETE").await;

    // The resource's physical id IS the group's name, and Auto Scaling answers for it: the
    // one call a client makes next with the value the stack just reported.
    let group_name = physical_id(created, &router, "Asg").await;
    let group = described_group(created, &router, &group_name).await;
    let group_arn = xml_text(&group, "AutoScalingGroupARN").unwrap_or_else(|| {
        panic!("case {created:?}: Auto Scaling must report the group's ARN, got {group}")
    });
    assert_eq!(
        xml_text(&group, "DesiredCapacity").as_deref(),
        Some(CAPACITY.to_string().as_str()),
        "case {created:?}: expected the declared desired capacity, got {group}"
    );
    // ...and the group really holds what it was sized for, which is what makes the stack's
    // completion a group a client can drive.
    let instances = in_service_instances(created, &router, &group_name, CAPACITY).await;
    for instance_id in &instances {
        assert!(
            is_service_assigned_id(instance_id, "i"),
            "case {created:?}: expected the instance ids Amazon EC2 assigned, got {instances:?}"
        );
    }

    assert_every_output(&router, created, &described, &group_name, &group_arn).await;

    // The same two reads after an in-place UPDATE of the group: a client deploys more than
    // once, and the capacity change is one the owning service applies to the group that
    // exists — so the identity the stack publishes does not move and every output still
    // carries its value.
    let updated = "the same stack after an in-place capacity update";
    let (status, body) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", STACK),
            member("TemplateBody", &template(UPDATED_CAPACITY)),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {updated:?}: UpdateStack must answer 200, got {status} {body}"
    );
    let described = settled_on(updated, &router, "UPDATE_COMPLETE").await;
    let after = physical_id(updated, &router, "Asg").await;
    assert_eq!(
        after, group_name,
        "case {updated:?}: an in-place update must leave the group's identity where it was, \
         got {after:?}"
    );
    let group = described_group(updated, &router, &after).await;
    assert_eq!(
        xml_text(&group, "DesiredCapacity").as_deref(),
        Some(UPDATED_CAPACITY.to_string().as_str()),
        "case {updated:?}: expected the updated desired capacity in the group itself, got \
         {group}"
    );
    assert_every_output(&router, updated, &described, &group_name, &group_arn).await;
}
