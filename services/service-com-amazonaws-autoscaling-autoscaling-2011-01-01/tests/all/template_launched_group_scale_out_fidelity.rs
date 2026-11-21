// SPDX-License-Identifier: CC0-1.0

//! What a client sees when it scales a group that launches from a LAUNCH TEMPLATE: the
//! sentence a capacity outside the group's own bounds is refused with, the launch-template
//! identity the instances such a change launches carry, and the sentence a template whose
//! security groups are outside the group's network is refused with.
//!
//! Three rules, each read where a client reads it — over the wire, through the service that
//! owns the resource:
//!
//! * RULE 1. `SetDesiredCapacity` asks for a capacity the group's OWN bounds admit, so one
//!   outside them is refused HTTP 400 `ValidationError` with the fixed text `New
//!   SetDesiredCapacity value <n> is above max value <MaxSize> for the AutoScalingGroup.`
//!   (`below min value <MinSize>` for the other bound): the requested capacity and the bound
//!   it crossed are the ONLY variable parts, the sentence ends at that period, and the group
//!   keeps the capacity it had. `SetDesiredCapacity` models only
//!   `ScalingActivityInProgressFault` and `ResourceContentionFault`
//!   (`smithy_query(query="operation",operation="SetDesiredCapacity")`), so the bound check
//!   is a service validation whose wording is the behavior clients match on.
//! * RULE 2. An instance a group launched FROM A LAUNCH TEMPLATE names the template it came
//!   from: beside `aws:autoscaling:groupName` and the group's `PropagateAtLaunch` tags it
//!   carries `aws:ec2launchtemplate:id` and `aws:ec2launchtemplate:version`, the version as
//!   the NUMBER the launch resolved rather than the `$Latest`/`$Default` word the group may
//!   float on. The launch is what stamps them, so a capacity change made AFTER the create
//!   stamps them exactly as the create's own capacity does.
//! * RULE 3. A security group belongs to one VPC (`SecurityGroup$VpcId`) and
//!   `VPCZoneIdentifier` names the subnets of the VPC a group launches into, so a template
//!   naming a group of another VPC is refused `ValidationError` with the entirely fixed
//!   sentence `One or more security groups in the launch template are not linked to the VPCs
//!   configured in the Auto Scaling group`, and nothing is stored.
//!
//! The rows differ along what each rule generalises over: which bound the capacity crosses
//! and by how much, bounds that are equal, a capacity ON each bound (served), whether the
//! change asks the group's cooldown to be honored, and the group's own name — which no
//! refusal carries; which operation drives the scale-out and how the group names its
//! template and version; and where the template names its security groups (inside a network
//! interface, where a template that declares one must put them) and whether a foreign group
//! stands alone or beside a group of the group's own VPC.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_ec2_amazonec2::state as ec2;

use crate::support::{
    borrowed, create_launch_template, ec2_query, emulator, p, query, seed_subnets, tag_value,
    tag_values, xml_unescape,
};

/// The tag Amazon EC2 Auto Scaling puts on every instance of a group.
const GROUP_NAME_TAG: &str = "aws:autoscaling:groupName";

/// The two tags a template launch carries, read from the service that OWNS launch templates
/// so a rename cannot leave this suite green against the old spelling.
const LAUNCH_TEMPLATE_ID_TAG: &str = ec2::LAUNCH_TEMPLATE_ID_TAG;
const LAUNCH_TEMPLATE_VERSION_TAG: &str = ec2::LAUNCH_TEMPLATE_VERSION_TAG;

/// The sentence a template whose security groups are outside the group's network is refused
/// with: entirely fixed text, naming neither the groups, the subnets nor the VPCs.
const FOREIGN_SECURITY_GROUP_MESSAGE: &str = "One or more security groups in the launch \
                                              template are not linked to the VPCs configured \
                                              in the Auto Scaling group";

/// The code Amazon EC2 Auto Scaling refuses a request it cannot satisfy with.
const VALIDATION_ERROR: &str = "ValidationError";

/// The `(code, message)` of a refused awsQuery answer, as a client parses them.
fn refusal(body: &str) -> (String, String) {
    (
        tag_value(body, "Code"),
        xml_unescape(&tag_value(body, "Message")),
    )
}

/// The fixed text `SetDesiredCapacity` refuses a capacity outside the group's own bounds
/// with, written out here rather than taken from the crate's own builder — a test that
/// re-used the implementation's literal would assert nothing.
fn expected_bound_sentence(desired: i32, relation: &str, bound: i32) -> String {
    format!(
        "New SetDesiredCapacity value {desired} is {relation} value {bound} for the \
         AutoScalingGroup."
    )
}

/// One row of the capacity-bound table.
struct BoundRow {
    case: &'static str,
    /// The group's own name, which no refusal may carry whatever it is.
    group: &'static str,
    min: i32,
    max: i32,
    /// The capacity the group is created with, which is inside its bounds.
    created: i32,
    /// An in-bounds capacity set BEFORE the row's own change, which starts the group's
    /// cooldown; `None` for a row that scales straight from the created capacity.
    prior: Option<i32>,
    /// What `SetDesiredCapacity` asks for.
    requested: i32,
    /// Whether that change asks for the cooldown to be honored.
    honor_cooldown: bool,
    /// The bound the request crosses as `(relation, bound)`, or `None` for a capacity the
    /// group's bounds admit.
    crossed: Option<(&'static str, i32)>,
}

const BOUND_ROWS: &[BoundRow] = &[
    BoundRow {
        case: "one above a maximum of one",
        group: "tpl-bound-one-above-max",
        min: 0,
        max: 1,
        created: 0,
        prior: None,
        requested: 2,
        honor_cooldown: false,
        crossed: Some(("above max", 1)),
    },
    BoundRow {
        case: "far above a maximum of three",
        group: "tpl-bound-far-above-max",
        min: 0,
        max: 3,
        created: 0,
        prior: None,
        requested: 97,
        honor_cooldown: false,
        crossed: Some(("above max", 3)),
    },
    BoundRow {
        case: "one below a minimum of two",
        group: "tpl-bound-below-min",
        min: 2,
        max: 4,
        created: 2,
        prior: None,
        requested: 1,
        honor_cooldown: false,
        crossed: Some(("below min", 2)),
    },
    BoundRow {
        case: "zero below a minimum of one",
        group: "tpl-bound-zero-below-min",
        min: 1,
        max: 2,
        created: 1,
        prior: None,
        requested: 0,
        honor_cooldown: false,
        crossed: Some(("below min", 1)),
    },
    BoundRow {
        case: "one above bounds that are equal",
        group: "tpl-bound-equal-bounds",
        min: 2,
        max: 2,
        created: 2,
        prior: None,
        requested: 3,
        honor_cooldown: false,
        crossed: Some(("above max", 2)),
    },
    BoundRow {
        case: "above the maximum, asking for the cooldown of an earlier change to be honored",
        group: "tpl-bound-honor-cooldown",
        min: 0,
        max: 1,
        created: 0,
        prior: Some(1),
        requested: 2,
        honor_cooldown: true,
        crossed: Some(("above max", 1)),
    },
    BoundRow {
        case: "the maximum itself, which the group serves",
        group: "tpl-bound-on-max",
        min: 0,
        max: 2,
        created: 0,
        prior: None,
        requested: 2,
        honor_cooldown: false,
        crossed: None,
    },
    BoundRow {
        case: "the minimum itself, which the group serves",
        group: "tpl-bound-on-min",
        min: 1,
        max: 3,
        created: 3,
        prior: None,
        requested: 1,
        honor_cooldown: false,
        crossed: None,
    },
];

#[tokio::test]
async fn set_desired_capacity_refuses_a_capacity_outside_the_groups_own_bounds() {
    let (router, engine) = emulator();
    let subnet = seed_subnets(&engine, 1)
        .first()
        .expect("one seeded subnet")
        .id
        .clone();
    let (template_id, _) = create_launch_template(&router, "lt-capacity-bounds").await;

    for row in BOUND_ROWS {
        let case = row.case;
        let params = vec![
            p("AutoScalingGroupName", row.group),
            p("MinSize", row.min.to_string()),
            p("MaxSize", row.max.to_string()),
            p("DesiredCapacity", row.created.to_string()),
            p("LaunchTemplate.LaunchTemplateId", template_id.clone()),
            p("VPCZoneIdentifier", subnet.clone()),
        ];
        let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateAutoScalingGroup must answer 200, got {status}: {body}"
        );

        // An in-bounds change first, where the row asks for one: it leaves the group in its
        // cooldown, which is the state the row's own change is made in.
        let mut held = row.created;
        if let Some(prior) = row.prior {
            let (status, body) = query(
                &router,
                "SetDesiredCapacity",
                &[
                    ("AutoScalingGroupName", row.group),
                    ("DesiredCapacity", &prior.to_string()),
                ],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the in-bounds change to {prior} must be served, got \
                 {status}: {body}"
            );
            held = prior;
        }

        let mut change = vec![
            p("AutoScalingGroupName", row.group),
            p("DesiredCapacity", row.requested.to_string()),
        ];
        if row.honor_cooldown {
            change.push(p("HonorCooldown", "true"));
        }
        let (status, body) = query(&router, "SetDesiredCapacity", &borrowed(&change)).await;

        let expected_capacity = match row.crossed {
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: a capacity the bounds admit must be served, got \
                     {status}: {body}"
                );
                row.requested
            }
            Some((relation, bound)) => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: a capacity outside the bounds must be refused, got \
                     {status}: {body}"
                );
                let answered = refusal(&body);
                assert_eq!(
                    answered,
                    (
                        VALIDATION_ERROR.to_string(),
                        expected_bound_sentence(row.requested, relation, bound)
                    ),
                    "case {case:?}: expected the {relation} sentence for {} against {bound}, \
                     got {answered:?}",
                    row.requested
                );
                // The sentence is the fixed text plus the two numbers: no name or id of the
                // request reaches it.
                for value in [row.group, template_id.as_str(), subnet.as_str()] {
                    assert!(
                        !answered.1.contains(value),
                        "case {case:?}: the refusal names nothing of the request, but \
                         {value:?} is in {:?}",
                        answered.1
                    );
                }
                held
            }
        };

        let (status, described) = query(
            &router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", row.group)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeAutoScalingGroups must answer 200, got {status}: {described}"
        );
        assert_eq!(
            tag_value(&described, "DesiredCapacity"),
            expected_capacity.to_string(),
            "case {case:?}: expected the group to hold {expected_capacity}, got {described}"
        );
    }
}

/// How the group names its launch template, and which operation scales it out.
#[derive(Debug, Clone, Copy)]
enum ScaledBy {
    /// `SetDesiredCapacity`, the operation a manual scale-out is made with.
    SetDesiredCapacity,
    /// `UpdateAutoScalingGroup`, which changes the same member of the same group.
    UpdateAutoScalingGroup,
}

impl ScaledBy {
    fn action(self) -> &'static str {
        match self {
            Self::SetDesiredCapacity => "SetDesiredCapacity",
            Self::UpdateAutoScalingGroup => "UpdateAutoScalingGroup",
        }
    }
}

/// One row of the launch-identity table.
struct IdentityRow {
    case: &'static str,
    group: &'static str,
    template: &'static str,
    /// How many versions the template has when the group scales out, so a row can float on
    /// a word over a template that has moved.
    versions: usize,
    /// Whether the group names the template by id; by name otherwise.
    by_id: bool,
    /// The `Version` the group names, or `None` when it names none.
    version: Option<&'static str>,
    scaled_by: ScaledBy,
    /// The version number the instance's tag must carry.
    launched_version: &'static str,
}

const IDENTITY_ROWS: &[IdentityRow] = &[
    IdentityRow {
        case: "a scale-out by SetDesiredCapacity, on a template pinned by id and version",
        group: "tpl-identity-pinned",
        template: "lt-identity-pinned",
        versions: 1,
        by_id: true,
        version: Some("1"),
        scaled_by: ScaledBy::SetDesiredCapacity,
        launched_version: "1",
    },
    IdentityRow {
        case: "a scale-out by UpdateAutoScalingGroup, on a template named by name and \
               floating on $Latest over two versions",
        group: "tpl-identity-latest",
        template: "lt-identity-latest",
        versions: 2,
        by_id: false,
        version: Some("$Latest"),
        scaled_by: ScaledBy::UpdateAutoScalingGroup,
        launched_version: "2",
    },
    IdentityRow {
        case: "a scale-out by SetDesiredCapacity, on a template that names no version at all",
        group: "tpl-identity-unversioned",
        template: "lt-identity-unversioned",
        versions: 2,
        by_id: true,
        version: None,
        scaled_by: ScaledBy::SetDesiredCapacity,
        launched_version: "1",
    },
];

/// The tag the group propagates to the instances it launches, which must be there beside the
/// service's own.
const PROPAGATED_TAG: (&str, &str) = ("workload", "scale-out");

#[tokio::test]
async fn an_instance_a_scale_out_launched_names_the_template_version_it_came_from() {
    for row in IDENTITY_ROWS {
        let case = row.case;
        let (router, engine) = emulator();
        let subnet = seed_subnets(&engine, 1)
            .first()
            .expect("one seeded subnet")
            .id
            .clone();
        let (template_id, _) = create_launch_template(&router, row.template).await;
        for _ in 1..row.versions {
            let (status, body) = ec2_query(
                &router,
                "CreateLaunchTemplateVersion",
                &[
                    p("LaunchTemplateId", template_id.clone()),
                    p("LaunchTemplateData.ImageId", "ami-0abcdef1234567891"),
                    p("LaunchTemplateData.InstanceType", "t3.small"),
                ],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: CreateLaunchTemplateVersion must answer 200, got {status}: {body}"
            );
        }

        // The group starts at zero capacity, so no instance exists before the scale-out: the
        // tags read below are the ones THAT change stamped.
        let mut params = vec![
            p("AutoScalingGroupName", row.group),
            p("MinSize", "0"),
            p("MaxSize", "1"),
            p("DesiredCapacity", "0"),
            p("VPCZoneIdentifier", subnet.clone()),
            p("Tags.member.1.Key", PROPAGATED_TAG.0),
            p("Tags.member.1.Value", PROPAGATED_TAG.1),
            p("Tags.member.1.PropagateAtLaunch", "true"),
        ];
        if row.by_id {
            params.push(p("LaunchTemplate.LaunchTemplateId", template_id.clone()));
        } else {
            params.push(p("LaunchTemplate.LaunchTemplateName", row.template));
        }
        if let Some(version) = row.version {
            params.push(p("LaunchTemplate.Version", version));
        }
        let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateAutoScalingGroup must answer 200, got {status}: {body}"
        );

        let (status, body) = query(
            &router,
            row.scaled_by.action(),
            &[
                ("AutoScalingGroupName", row.group),
                ("DesiredCapacity", "1"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: {} must answer 200, got {status}: {body}",
            row.scaled_by.action()
        );

        let instance_id = launched_instance(&router, case, row.group).await;
        let tags = instance_tags(&router, case, &instance_id).await;
        for (tag, expected) in [
            (LAUNCH_TEMPLATE_ID_TAG, template_id.as_str()),
            (LAUNCH_TEMPLATE_VERSION_TAG, row.launched_version),
            (GROUP_NAME_TAG, row.group),
            (PROPAGATED_TAG.0, PROPAGATED_TAG.1),
        ] {
            let held = tags
                .iter()
                .find(|(key, _)| key == tag)
                .map(|(_, value)| value.clone());
            assert_eq!(
                held.as_deref(),
                Some(expected),
                "case {case:?}: expected the instance to carry {tag}={expected:?}, got {tags:?}"
            );
        }
    }
}

/// Where the foreign security group sits in the row's template, and what the create must
/// answer.
struct NetworkRow {
    case: &'static str,
    /// Whether the network interface names a group of the group's OWN VPC.
    own_group: bool,
    /// Whether it names a group of ANOTHER VPC.
    foreign_group: bool,
    /// Whether the create is refused.
    refused: bool,
}

const NETWORK_ROWS: &[NetworkRow] = &[
    NetworkRow {
        case: "a network interface naming a security group of the group's own VPC",
        own_group: true,
        foreign_group: false,
        refused: false,
    },
    NetworkRow {
        case: "a network interface naming a security group of another VPC",
        own_group: false,
        foreign_group: true,
        refused: true,
    },
    NetworkRow {
        case: "a network interface naming a group of each VPC",
        own_group: true,
        foreign_group: true,
        refused: true,
    },
];

#[tokio::test]
async fn a_network_interfaces_security_groups_must_sit_in_the_groups_own_vpc() {
    for (index, row) in NETWORK_ROWS.iter().enumerate() {
        let case = row.case;
        let (router, _engine) = emulator();
        let vpc = ec2_value(&router, case, "CreateVpc", &[p("CidrBlock", "10.31.0.0/16")], "vpcId")
            .await;
        let subnet = ec2_value(
            &router,
            case,
            "CreateSubnet",
            &[
                p("VpcId", vpc.clone()),
                p("CidrBlock", "10.31.1.0/24"),
                p("AvailabilityZone", "us-west-2a"),
            ],
            "subnetId",
        )
        .await;
        let own = ec2_value(
            &router,
            case,
            "CreateSecurityGroup",
            &[
                p("GroupName", format!("interface-own-{index}")),
                p("GroupDescription", "the groups own network"),
                p("VpcId", vpc.clone()),
            ],
            "groupId",
        )
        .await;
        let other_vpc =
            ec2_value(&router, case, "CreateVpc", &[p("CidrBlock", "10.32.0.0/16")], "vpcId").await;
        let foreign = ec2_value(
            &router,
            case,
            "CreateSecurityGroup",
            &[
                p("GroupName", format!("interface-foreign-{index}")),
                p("GroupDescription", "another network of the account"),
                p("VpcId", other_vpc.clone()),
            ],
            "groupId",
        )
        .await;

        // A template that declares a network interface carries its security groups INSIDE
        // that interface, as `LaunchTemplateInstanceNetworkInterfaceSpecificationRequest$Groups`
        // publishes them under the `SecurityGroupId` member name.
        let template = format!("lt-interface-{index}");
        let mut data = vec![
            p("LaunchTemplateName", template.clone()),
            p("LaunchTemplateData.ImageId", "ami-00000000000000071"),
            p("LaunchTemplateData.InstanceType", "t3.micro"),
            p("LaunchTemplateData.NetworkInterface.1.DeviceIndex", "0"),
        ];
        let mut position = 0;
        for (wanted, group) in [(row.own_group, &own), (row.foreign_group, &foreign)] {
            if !wanted {
                continue;
            }
            position += 1;
            data.push(p(
                &format!("LaunchTemplateData.NetworkInterface.1.SecurityGroupId.{position}"),
                group.clone(),
            ));
        }
        let (status, body) = ec2_query(&router, "CreateLaunchTemplate", &data).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateLaunchTemplate must answer 200, got {status}: {body}"
        );

        let group_name = format!("interface-network-{index}");
        let params = vec![
            p("AutoScalingGroupName", group_name.clone()),
            p("MinSize", "0"),
            p("MaxSize", "1"),
            p("LaunchTemplate.LaunchTemplateName", template.clone()),
            p("VPCZoneIdentifier", subnet.clone()),
        ];
        let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;

        if !row.refused {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the create to be accepted, got {status}: {body}"
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {case:?}: expected the create to be refused, got {status}: {body}"
        );
        let answered = refusal(&body);
        assert_eq!(
            answered,
            (
                VALIDATION_ERROR.to_string(),
                FOREIGN_SECURITY_GROUP_MESSAGE.to_string()
            ),
            "case {case:?}: expected the foreign-security-group refusal, got {answered:?}"
        );
        for value in [&own, &foreign, &subnet, &vpc, &other_vpc] {
            assert!(
                !answered.1.contains(value.as_str()),
                "case {case:?}: the sentence names no resource of the request, but {value:?} \
                 is in {:?}",
                answered.1
            );
        }

        let (status, described) = query(
            &router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", group_name.as_str())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeAutoScalingGroups must answer 200, got {status}: {described}"
        );
        assert!(
            !described.contains(group_name.as_str()),
            "case {case:?}: a refused create leaves no group behind, got {described}"
        );
    }
}

/// The one element of an answered ec2Query body this suite reads, after asserting the call
/// was served.
async fn ec2_value(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(String, String)],
    element: &str,
) -> String {
    let (status, body) = ec2_query(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {action} must answer 200, got {status}: {body}"
    );
    let value = tag_value(&body, element);
    assert!(
        !value.is_empty(),
        "case {case:?}: {action} must report its {element}, got {body}"
    );
    value
}

/// The id of the one instance the group holds, polled until this service reports it.
///
/// What this waits for is the CONDITION — the group reporting an instance — under a deadline
/// far longer than the span this emulator gives a launch.
async fn launched_instance(router: &MultiServiceRouter, case: &str, group: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let (status, body) = query(
            router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", group)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeAutoScalingGroups must answer 200, got {status}: {body}"
        );
        let reported = tag_value(&body, "InstanceId");
        if !reported.is_empty() {
            return reported;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the scale-out to launch an instance, got {body}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// The tags Amazon EC2 reports for ONE instance, as `(key, value)` pairs, polled until that
/// service resolves the instance the group reports.
async fn instance_tags(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> Vec<(String, String)> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let (status, body) =
            ec2_query(router, "DescribeInstances", &[p("InstanceId.1", instance_id)]).await;
        if status == 200 && tag_value(&body, "instanceId") == instance_id {
            return tag_values(&body, "key")
                .into_iter()
                .zip(tag_values(&body, "value"))
                .collect();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: Amazon EC2 never resolved {instance_id}, which the group reports \
             as its instance: last answer {status}: {body}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
