// SPDX-License-Identifier: CC0-1.0

//! The instances an Auto Scaling group reports are the account's own Amazon EC2
//! instances.
//!
//! A capacity change on a group STARTS A SCALING ACTIVITY THAT LAUNCHES INSTANCES: the
//! ids the group then reports under `AutoScalingGroups[].Instances[].InstanceId` name
//! instances of the account, so Amazon EC2's own `DescribeInstances` resolves every one
//! of them — running, from the launch source's image and instance type, in a subnet of
//! the group's `VPCZoneIdentifier` and carrying the group's propagate-at-launch tags —
//! and a scale-in moves those same ids to `terminated`. An id that resolved in no EC2
//! state would be an id no client could inspect, tag, connect to or terminate.
//!
//! The table below spans what that rule generalises over: the launch source (a launch
//! template named by name, by id, or through a mixed instances policy whose override
//! names the instance type), how many placements the group has (one, two or three
//! subnets, or zones with no subnet at all), how much capacity is asked for, whether it
//! is asked for by the create or by a later `SetDesiredCapacity`, and how many of the
//! group's tags propagate at launch.
//!
//! Both services are mounted on ONE engine and driven over their own wire protocols —
//! awsQuery for Auto Scaling, ec2Query for Amazon EC2 — the way the emulator serves
//! them, so the ids these assertions follow are the ids a client is given.
//!
//! The suites below follow the same rule through the three other ways a group's membership
//! is decided: how many instances a capacity stated in vCPUs or in memory needs, what
//! happens to a member Amazon EC2 stops running, and the pace an instance refresh replaces
//! them at.

use std::sync::Arc;
use std::time::Duration;

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;
use service_com_amazonaws_ec2_amazonec2 as ec2_service;
use service_com_amazonaws_ec2_amazonec2::state as ec2;

use crate::support::{
    ec2_query, p, query, seed_subnets, tag_value, tag_values, xml_unescape, REGION,
};

/// Wait until every clock-owed transition started before now has certainly fallen due —
/// this service's own lifecycle span and Amazon EC2's, whichever is longer, plus a
/// margin. Derived from the services' own spans, not written out, so the three cannot
/// drift apart.
async fn past_the_transitions() {
    let span = autoscaling::state::LIFECYCLE_TRANSITION.max(ec2::TRANSITION_DELAY);
    tokio::time::sleep(span + Duration::from_millis(250)).await;
}

/// One emulator with BOTH services on one engine: an Auto Scaling group's instances
/// live in the same account's Amazon EC2 store, so one engine is what makes the two
/// views one account.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(autoscaling::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(ec2_service::EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// What Amazon EC2 reports for one instance, read out of a `DescribeInstances` answer
/// that named exactly that instance.
#[derive(Debug, Clone, Default)]
struct DescribedInstance {
    instance_id: String,
    state: String,
    image_id: String,
    instance_type: String,
    subnet_id: String,
    availability_zone: String,
    /// The instance's tags, as `key=value` pairs in the order reported.
    tags: Vec<String>,
}

/// Ask Amazon EC2 about ONE instance id, so every member of the answer belongs to that
/// instance. Answers `None` when EC2 refuses the id — which is what an id that resolves
/// in no EC2 state does.
async fn described_instance(
    router: &MultiServiceRouter,
    instance_id: &str,
) -> Option<DescribedInstance> {
    let (status, body) = ec2_query(router, "DescribeInstances", &[p("InstanceId.1", instance_id)]).await;
    if status != 200 {
        return None;
    }
    let keys = tag_values(&body, "key");
    let values = tag_values(&body, "value");
    Some(DescribedInstance {
        instance_id: tag_value(&body, "instanceId"),
        state: tag_value(&tag_value(&body, "instanceState"), "name"),
        image_id: tag_value(&body, "imageId"),
        instance_type: tag_value(&body, "instanceType"),
        subnet_id: tag_value(&body, "subnetId"),
        availability_zone: tag_value(&tag_value(&body, "placement"), "availabilityZone"),
        tags: keys
            .iter()
            .zip(values.iter())
            .map(|(key, value)| format!("{key}={value}"))
            .collect(),
    })
}

/// Create one Amazon EC2 launch template over the wire and answer its id.
async fn create_launch_template(
    router: &MultiServiceRouter,
    name: &str,
    image_id: &str,
    instance_type: &str,
) -> String {
    let (status, body) = ec2_query(
        router,
        "CreateLaunchTemplate",
        &[
            p("LaunchTemplateName", name),
            p("LaunchTemplateData.ImageId", image_id),
            p("LaunchTemplateData.InstanceType", instance_type),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateLaunchTemplate for {name} expected 200, got {status}: {body}"
    );
    let id = tag_value(&body, "launchTemplateId");
    assert!(
        !id.is_empty(),
        "CreateLaunchTemplate must report the template's id, got {body}"
    );
    id
}

/// How the group names its launch source.
#[derive(Debug, Clone, Copy)]
enum Source {
    /// `LaunchTemplate` naming the template by name.
    TemplateByName,
    /// `LaunchTemplate` naming the template by id.
    TemplateById,
    /// `MixedInstancesPolicy` whose launch template is the one created, with one
    /// override naming the instance type to launch.
    MixedPolicyOverriding(&'static str),
}

/// How the capacity is asked for.
#[derive(Debug, Clone, Copy)]
enum AskedBy {
    /// `CreateAutoScalingGroup`'s own `DesiredCapacity`.
    Create,
    /// A later `SetDesiredCapacity`.
    SetDesiredCapacity,
}

/// One row of the launch table.
struct Row {
    case: &'static str,
    /// How many subnets the group's `VPCZoneIdentifier` names; 0 names zones instead.
    subnets: usize,
    source: Source,
    image_id: &'static str,
    /// The instance type the launch template carries.
    instance_type: &'static str,
    /// The instance type the group's instances must run, which an override replaces.
    expected_instance_type: &'static str,
    capacity: i32,
    asked_by: AskedBy,
    /// The group's tags, as `(key, value, propagate at launch)`.
    tags: &'static [(&'static str, &'static str, bool)],
}

/// Every instance id an Auto Scaling group reports resolves in the account's Amazon EC2
/// state, with the launch source's parameters and a placement of the group — and a
/// scale-in terminates those same instances.
#[tokio::test]
async fn reported_instances_are_the_accounts_own_ec2_instances() {
    let rows = [
        Row {
            case: "one subnet, template by name, capacity asked for by the create",
            subnets: 1,
            source: Source::TemplateByName,
            image_id: "ami-00000000000000011",
            instance_type: "t3.micro",
            expected_instance_type: "t3.micro",
            capacity: 1,
            asked_by: AskedBy::Create,
            tags: &[],
        },
        Row {
            case: "two subnets, template by id, capacity set after the create, two tags",
            subnets: 2,
            source: Source::TemplateById,
            image_id: "ami-00000000000000012",
            instance_type: "t3.small",
            expected_instance_type: "t3.small",
            capacity: 2,
            asked_by: AskedBy::SetDesiredCapacity,
            tags: &[("env", "prod", true), ("team", "core", false)],
        },
        Row {
            case: "three subnets, a mixed instances policy overriding the instance type",
            subnets: 3,
            source: Source::MixedPolicyOverriding("m5.large"),
            image_id: "ami-00000000000000013",
            instance_type: "t3.micro",
            expected_instance_type: "m5.large",
            capacity: 3,
            asked_by: AskedBy::SetDesiredCapacity,
            tags: &[("Name", "web", true)],
        },
        Row {
            case: "no subnets: a group placed by Availability Zone alone",
            subnets: 0,
            source: Source::TemplateByName,
            image_id: "ami-00000000000000014",
            instance_type: "c5.xlarge",
            expected_instance_type: "c5.xlarge",
            capacity: 2,
            asked_by: AskedBy::Create,
            tags: &[("stage", "", true)],
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        // One emulator per row: each row is its own account, so the ids and addresses
        // of one row never explain another's.
        let (router, engine) = emulator();
        let subnets = seed_subnets(&engine, row.subnets);
        let template_name = format!("lt-{index}");
        let template_id =
            create_launch_template(&router, &template_name, row.image_id, row.instance_type).await;

        let name = format!("asg-{index}");
        let capacity = row.capacity.to_string();
        let identifier = subnets
            .iter()
            .map(|subnet| subnet.id.as_str())
            .collect::<Vec<_>>()
            .join(",");
        // The zone a subnetless group is placed by: the first of the Region's own
        // catalog, never written out here.
        let fallback_zone = ec2::availability_zones_in_region(REGION)
            .first()
            .map(|zone| zone.zone_name.clone())
            .expect("the Region has Availability Zones");

        let mut params: Vec<(String, String)> = vec![
            p("AutoScalingGroupName", name.clone()),
            p("MinSize", "0"),
            p("MaxSize", row.capacity.to_string()),
        ];
        match row.source {
            Source::TemplateByName => {
                params.push(p("LaunchTemplate.LaunchTemplateName", template_name.clone()));
            }
            Source::TemplateById => {
                params.push(p("LaunchTemplate.LaunchTemplateId", template_id.clone()));
            }
            Source::MixedPolicyOverriding(instance_type) => {
                params.push(p(
                    "MixedInstancesPolicy.LaunchTemplate.LaunchTemplateSpecification.LaunchTemplateName",
                    template_name.clone(),
                ));
                params.push(p(
                    "MixedInstancesPolicy.LaunchTemplate.Overrides.member.1.InstanceType",
                    instance_type,
                ));
            }
        }
        if row.subnets > 0 {
            params.push(p("VPCZoneIdentifier", identifier.clone()));
        } else {
            params.push(p("AvailabilityZones.member.1", fallback_zone.clone()));
        }
        for (position, (key, value, propagate)) in row.tags.iter().enumerate() {
            let member = position + 1;
            params.push(p(&format!("Tags.member.{member}.Key"), *key));
            params.push(p(&format!("Tags.member.{member}.Value"), *value));
            params.push(p(
                &format!("Tags.member.{member}.PropagateAtLaunch"),
                if *propagate { "true" } else { "false" },
            ));
        }
        if matches!(row.asked_by, AskedBy::Create) {
            params.push(p("DesiredCapacity", capacity.clone()));
        }

        let borrowed: Vec<(&str, &str)> = params
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed).await;
        assert_eq!(
            status, 200,
            "case {:?}: CreateAutoScalingGroup expected 200, got {status}: {body}",
            row.case
        );

        if matches!(row.asked_by, AskedBy::SetDesiredCapacity) {
            let (status, body) = query(
                &router,
                "SetDesiredCapacity",
                &[
                    ("AutoScalingGroupName", name.as_str()),
                    ("DesiredCapacity", capacity.as_str()),
                ],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {:?}: SetDesiredCapacity expected 200, got {status}: {body}",
                row.case
            );
        }

        past_the_transitions().await;
        let (status, body) = query(
            &router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", name.as_str())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: DescribeAutoScalingGroups expected 200, got {status}: {body}",
            row.case
        );
        let reported = tag_values(&body, "InstanceId");
        assert_eq!(
            reported.len(),
            usize::try_from(row.capacity).expect("a capacity is a count"),
            "case {:?}: expected {} instances, got {reported:?} in {body}",
            row.case,
            row.capacity
        );

        // Every id the group reported is an instance of the account, running what the
        // group's launch source says and placed where the group places.
        let expected_zones: Vec<String> = if row.subnets > 0 {
            subnets.iter().map(|subnet| subnet.zone.clone()).collect()
        } else {
            vec![fallback_zone.clone()]
        };
        let expected_subnets: Vec<String> = subnets.iter().map(|subnet| subnet.id.clone()).collect();
        for instance_id in &reported {
            let described = described_instance(&router, instance_id)
                .await
                .unwrap_or_else(|| {
                    panic!(
                        "case {:?}: the group reports {instance_id}, which Amazon EC2 \
                         resolves to no instance",
                        row.case
                    )
                });
            assert_eq!(
                described.instance_id, *instance_id,
                "case {:?}: expected Amazon EC2 to report {instance_id}, got {described:?}",
                row.case
            );
            assert_eq!(
                described.state, "running",
                "case {:?}: expected {instance_id} running once the transition is due, \
                 got {described:?}",
                row.case
            );
            assert_eq!(
                described.image_id, row.image_id,
                "case {:?}: expected the launch source's image {}, got {described:?}",
                row.case, row.image_id
            );
            assert_eq!(
                described.instance_type, row.expected_instance_type,
                "case {:?}: expected the launch source's instance type {}, got {described:?}",
                row.case, row.expected_instance_type
            );
            assert!(
                expected_zones.contains(&described.availability_zone),
                "case {:?}: expected a zone of {expected_zones:?}, got {described:?}",
                row.case
            );
            if row.subnets > 0 {
                assert!(
                    expected_subnets.contains(&described.subnet_id),
                    "case {:?}: expected a subnet of the group's {expected_subnets:?}, \
                     got {described:?}",
                    row.case
                );
            } else {
                assert!(
                    described.subnet_id.is_empty(),
                    "case {:?}: a group that named no subnet places no instance in one, \
                     got {described:?}",
                    row.case
                );
            }
            // The tags marked PropagateAtLaunch are on the instance; the others are not.
            // Beside them, the group's own name, which Amazon EC2 Auto Scaling tags
            // every instance it launches with.
            for (key, value, propagate) in row.tags {
                let pair = format!("{key}={value}");
                assert_eq!(
                    described.tags.contains(&pair),
                    *propagate,
                    "case {:?}: the tag {pair:?} propagates at launch: {propagate}, \
                     got {described:?}",
                    row.case
                );
            }
            assert!(
                described.tags.contains(&format!(
                    "{}={name}",
                    autoscaling::state::ec2_instances::GROUP_NAME_TAG
                )),
                "case {:?}: an instance of the group carries the group's name, got {described:?}",
                row.case
            );
        }

        // Scaling back in terminates THOSE instances: the same ids, in Amazon EC2.
        let (status, body) = query(
            &router,
            "SetDesiredCapacity",
            &[
                ("AutoScalingGroupName", name.as_str()),
                ("DesiredCapacity", "0"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: scaling in expected 200, got {status}: {body}",
            row.case
        );
        past_the_transitions().await;
        for instance_id in &reported {
            let described = described_instance(&router, instance_id)
                .await
                .unwrap_or_else(|| {
                    panic!(
                        "case {:?}: a terminated instance is still reported by Amazon EC2, \
                         but {instance_id} resolves to none",
                        row.case
                    )
                });
            assert_eq!(
                described.state, "terminated",
                "case {:?}: expected {instance_id} terminated after the scale-in, \
                 got {described:?}",
                row.case
            );
        }
    }
}

/// A forced delete takes the group's instances with it: "the group is to be deleted
/// along with all instances associated with the group"
/// (`smithy_query(query="doc",shape="DeleteAutoScalingGroupType",member="ForceDelete")`),
/// so the account's own Amazon EC2 instances are terminated.
#[tokio::test]
async fn a_forced_delete_terminates_the_groups_ec2_instances() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 2);
    create_launch_template(&router, "lt-force", "ami-00000000000000021", "t3.micro").await;
    let identifier = format!("{},{}", subnets[0].id, subnets[1].id);

    let (status, body) = query(
        &router,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", "forced"),
            ("LaunchTemplate.LaunchTemplateName", "lt-force"),
            ("MinSize", "2"),
            ("MaxSize", "2"),
            ("VPCZoneIdentifier", identifier.as_str()),
        ],
    )
    .await;
    assert_eq!(status, 200, "CreateAutoScalingGroup: got {status}: {body}");

    past_the_transitions().await;
    let (_, body) = query(
        &router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", "forced")],
    )
    .await;
    let reported = tag_values(&body, "InstanceId");
    assert_eq!(
        reported.len(),
        2,
        "the group must hold the capacity it was created with, got {body}"
    );

    let (status, body) = query(
        &router,
        "DeleteAutoScalingGroup",
        &[
            ("AutoScalingGroupName", "forced"),
            ("ForceDelete", "true"),
        ],
    )
    .await;
    assert_eq!(status, 200, "a forced delete: got {status}: {body}");

    past_the_transitions().await;
    for instance_id in &reported {
        let described = described_instance(&router, instance_id)
            .await
            .unwrap_or_else(|| panic!("Amazon EC2 resolves {instance_id} to no instance"));
        assert_eq!(
            described.state, "terminated",
            "a forced delete terminates {instance_id}, got {described:?}"
        );
    }
}

/// How long a poll in the two suites below keeps looking before it gives up, and how
/// often it looks again.
///
/// A BACKSTOP, not a schedule: nothing inside the emulator sleeps for a due instant, so
/// each wait below looks again until the CONDITION it needs holds. Generous on purpose —
/// several of these run in parallel — and derived from the service's own spans so this and
/// the implementation cannot drift apart.
fn settle_deadline() -> Duration {
    (autoscaling::state::INSTANCE_REFRESH_REPLACEMENT
        .max(autoscaling::state::LIFECYCLE_TRANSITION)
        .max(ec2::TRANSITION_DELAY))
        * 40
}

/// How long one poll waits before looking again.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The instances one group reports, as `(instance id, lifecycle state, weighted capacity)`
/// triples in the order reported.
async fn reported_instances(
    router: &MultiServiceRouter,
    name: &str,
) -> Vec<(String, String, String)> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeAutoScalingGroups for {name} expected 200, got {status}: {body}"
    );
    let ids = tag_values(&body, "InstanceId");
    let states = tag_values(&body, "LifecycleState");
    let weights = tag_values(&body, "WeightedCapacity");
    ids.into_iter()
        .enumerate()
        .map(|(index, id)| {
            (
                id,
                states.get(index).cloned().unwrap_or_default(),
                weights.get(index).cloned().unwrap_or_default(),
            )
        })
        .collect()
}

/// Look again until the group holds `wanted` instances, every one of them `InService`, or
/// the deadline passes; answers what it last saw either way.
async fn settled_instances(
    router: &MultiServiceRouter,
    name: &str,
    wanted: usize,
) -> Vec<(String, String, String)> {
    let deadline = tokio::time::Instant::now() + settle_deadline();
    loop {
        let reported = reported_instances(router, name).await;
        let in_service = reported
            .iter()
            .filter(|(_, state, _)| state == "InService")
            .count();
        if reported.len() == wanted && in_service == wanted {
            return reported;
        }
        if tokio::time::Instant::now() >= deadline {
            return reported;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One row of the capacity-unit table: the unit the group's sizes are stated in, how its
/// launch source picks the instance type, and the capacity asked for in that unit.
struct CapacityUnitRow {
    case: &'static str,
    /// `DesiredCapacityType`, or `None` to leave it out — which is the default, "`units`,
    /// which translates into number of instances".
    desired_capacity_type: Option<&'static str>,
    /// The instance type the override names outright, when it names one.
    override_instance_type: Option<&'static str>,
    /// The `InstanceRequirements$VCpuCount` the override states instead, as `(min, max)`.
    required_vcpus: Option<(i32, i32)>,
    /// The `InstanceRequirements$AllowedInstanceTypes` patterns, which may carry wild
    /// cards.
    allowed_instance_types: &'static [&'static str],
    /// The capacity asked for, in the group's own unit.
    capacity: i32,
}

/// A group's `DesiredCapacity` is fulfilled in the unit its `DesiredCapacityType` names:
/// each instance provides its own vCPUs or its own memory, so the group launches as many
/// instances as that budget needs — and with `InstanceRequirements` instead of an
/// `InstanceType`, the type it launches is one the requirements admit.
///
/// Every expectation here is DERIVED from Amazon EC2's own published facts for the type the
/// group actually launched ([`ec2::instance_type_record`], the service that owns instance
/// types), so no row asserts a vCPU count or a memory size written out by hand.
#[tokio::test]
async fn a_groups_capacity_is_counted_in_the_unit_its_desired_capacity_type_names() {
    let rows = [
        CapacityUnitRow {
            case: "the default unit: the capacity is an instance count",
            desired_capacity_type: None,
            override_instance_type: Some("m5.large"),
            required_vcpus: None,
            allowed_instance_types: &[],
            capacity: 2,
        },
        CapacityUnitRow {
            case: "vcpu: a budget the instances' vCPUs divide exactly",
            desired_capacity_type: Some(
                autoscaling::state::instance_requirements::CAPACITY_TYPE_VCPU,
            ),
            override_instance_type: Some("m5.large"),
            required_vcpus: None,
            allowed_instance_types: &[],
            capacity: 4,
        },
        CapacityUnitRow {
            case: "vcpu: a budget they do not divide, so the last launch overshoots it",
            desired_capacity_type: Some(
                autoscaling::state::instance_requirements::CAPACITY_TYPE_VCPU,
            ),
            override_instance_type: Some("m5.large"),
            required_vcpus: None,
            allowed_instance_types: &[],
            capacity: 5,
        },
        CapacityUnitRow {
            case: "vcpu with attribute-based selection: two-vCPU types of two families",
            desired_capacity_type: Some(
                autoscaling::state::instance_requirements::CAPACITY_TYPE_VCPU,
            ),
            override_instance_type: None,
            required_vcpus: Some((2, 2)),
            allowed_instance_types: &["c5.*", "m5.*"],
            capacity: 4,
        },
        CapacityUnitRow {
            case: "vcpu with attribute-based selection: four-vCPU types of one family",
            desired_capacity_type: Some(
                autoscaling::state::instance_requirements::CAPACITY_TYPE_VCPU,
            ),
            override_instance_type: None,
            required_vcpus: Some((4, 4)),
            allowed_instance_types: &["c5.*"],
            capacity: 8,
        },
        CapacityUnitRow {
            case: "memory-mib: the capacity is a memory budget",
            desired_capacity_type: Some(
                autoscaling::state::instance_requirements::CAPACITY_TYPE_MEMORY_MIB,
            ),
            override_instance_type: Some("t3.nano"),
            required_vcpus: None,
            allowed_instance_types: &[],
            capacity: 1_000,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        // One emulator per row: each row is its own account, so the ids of one row never
        // explain another's.
        let (router, engine) = emulator();
        let subnets = seed_subnets(&engine, 2);
        let template_name = format!("lt-units-{index}");
        // The template names a type of its own, with a vCPU count none of the rows asks
        // for: an override that states requirements decides the type whatever the template
        // carries.
        create_launch_template(
            &router,
            &template_name,
            "ami-00000000000000041",
            "m5.4xlarge",
        )
        .await;

        let name = format!("asg-units-{index}");
        let capacity = row.capacity.to_string();
        let identifier = format!("{},{}", subnets[0].id, subnets[1].id);
        let mut params: Vec<(String, String)> = vec![
            p("AutoScalingGroupName", name.clone()),
            p("MinSize", "0"),
            p("MaxSize", capacity.clone()),
            p("DesiredCapacity", capacity.clone()),
            p("VPCZoneIdentifier", identifier.clone()),
            p(
                "MixedInstancesPolicy.LaunchTemplate.LaunchTemplateSpecification.LaunchTemplateName",
                template_name.clone(),
            ),
        ];
        if let Some(instance_type) = row.override_instance_type {
            params.push(p(
                "MixedInstancesPolicy.LaunchTemplate.Overrides.member.1.InstanceType",
                instance_type,
            ));
        }
        if let Some((min, max)) = row.required_vcpus {
            params.push(p(
                "MixedInstancesPolicy.LaunchTemplate.Overrides.member.1.InstanceRequirements.VCpuCount.Min",
                min.to_string(),
            ));
            params.push(p(
                "MixedInstancesPolicy.LaunchTemplate.Overrides.member.1.InstanceRequirements.VCpuCount.Max",
                max.to_string(),
            ));
            params.push(p(
                "MixedInstancesPolicy.LaunchTemplate.Overrides.member.1.InstanceRequirements.MemoryMiB.Min",
                "1024",
            ));
        }
        for (position, pattern) in row.allowed_instance_types.iter().enumerate() {
            let member = position + 1;
            params.push(p(
                &format!(
                    "MixedInstancesPolicy.LaunchTemplate.Overrides.member.1.InstanceRequirements.\
                     AllowedInstanceTypes.member.{member}"
                ),
                *pattern,
            ));
        }
        if let Some(desired_capacity_type) = row.desired_capacity_type {
            params.push(p("DesiredCapacityType", desired_capacity_type));
        }
        let borrowed: Vec<(&str, &str)> = params
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed).await;
        assert_eq!(
            status, 200,
            "case {:?}: CreateAutoScalingGroup expected 200, got {status}: {body}",
            row.case
        );

        // What the group launched, and what Amazon EC2 says those instances are.
        let reported = reported_instances(&router, &name).await;
        assert!(
            !reported.is_empty(),
            "case {:?}: the group must launch what its capacity needs, got {reported:?}",
            row.case
        );
        let mut launched_types: Vec<String> = Vec::new();
        for (instance_id, _, _) in &reported {
            let described = described_instance(&router, instance_id)
                .await
                .unwrap_or_else(|| {
                    panic!(
                        "case {:?}: the group reports {instance_id}, which Amazon EC2 resolves \
                         to no instance",
                        row.case
                    )
                });
            launched_types.push(described.instance_type);
        }
        let launched_type = launched_types[0].clone();
        assert!(
            launched_types.iter().all(|kind| *kind == launched_type),
            "case {:?}: one launch source launches one type, got {launched_types:?}",
            row.case
        );
        let record = ec2::instance_type_record(&launched_type).unwrap_or_else(|| {
            panic!(
                "case {:?}: Amazon EC2 publishes no facts for the launched type \
                 {launched_type:?}",
                row.case
            )
        });

        // The units ONE of those instances provides, in the unit this group counts in.
        let units_per_instance = match row.desired_capacity_type {
            Some(unit) if unit == autoscaling::state::instance_requirements::CAPACITY_TYPE_VCPU => {
                record.default_vcpus
            }
            Some(unit)
                if unit
                    == autoscaling::state::instance_requirements::CAPACITY_TYPE_MEMORY_MIB =>
            {
                i32::try_from(record.memory_mib).expect("a published memory size fits in an i32")
            }
            _ => 1,
        };
        assert!(
            units_per_instance > 0,
            "case {:?}: an instance of {launched_type:?} must provide capacity, got \
             {units_per_instance}",
            row.case
        );
        // "Amazon EC2 Auto Scaling launches instances until the desired capacity is totally
        // fulfilled, even if this results in an overage"
        // (`LaunchTemplateOverrides$WeightedCapacity`), so the count is the budget divided
        // by what one instance provides, ROUNDED UP.
        let expected = (row.capacity + units_per_instance - 1) / units_per_instance;
        assert_eq!(
            i32::try_from(reported.len()).expect("an instance count fits in an i32"),
            expected,
            "case {:?}: {} units of {launched_type:?} ({units_per_instance} each) need \
             {expected} instances, got {reported:?}",
            row.case,
            row.capacity
        );
        let provided = expected * units_per_instance;
        assert!(
            provided >= row.capacity && provided - units_per_instance < row.capacity,
            "case {:?}: {expected} instances provide {provided} units, which must fulfil \
             {} without a whole instance to spare",
            row.case,
            row.capacity
        );

        // With requirements instead of a type, the type launched is one they admit: its
        // vCPUs are in range and its name matches one of the allowed patterns.
        if let Some((min, max)) = row.required_vcpus {
            assert!(
                record.default_vcpus >= min && record.default_vcpus <= max,
                "case {:?}: the requirements admit {min}..={max} vCPUs, but {launched_type:?} \
                 has {}",
                row.case,
                record.default_vcpus
            );
            let family = launched_type
                .split('.')
                .next()
                .expect("an instance type has a family")
                .to_string();
            assert!(
                row.allowed_instance_types
                    .iter()
                    .any(|pattern| pattern.trim_end_matches(".*") == family),
                "case {:?}: the requirements allow {:?}, but {launched_type:?} is of none of \
                 them",
                row.case,
                row.allowed_instance_types
            );
        }

        // The group reports the budget and the unit it was asked for, unchanged, and each
        // member reports the units it contributes.
        let (_, body) = query(
            &router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", name.as_str())],
        )
        .await;
        assert_eq!(
            tag_value(&body, "DesiredCapacity"),
            capacity,
            "case {:?}: the group reports the budget it was asked for, got {body}",
            row.case
        );
        assert_eq!(
            tag_value(&body, "DesiredCapacityType"),
            row.desired_capacity_type.unwrap_or_default(),
            "case {:?}: the group reports the unit it was asked for, got {body}",
            row.case
        );
        let weights = tag_values(&body, "WeightedCapacity");
        if row.desired_capacity_type.is_some() {
            assert_eq!(
                weights,
                vec![units_per_instance.to_string(); reported.len()],
                "case {:?}: each member reports the units it contributes, got {body}",
                row.case
            );
        } else {
            assert!(
                weights.is_empty(),
                "case {:?}: a group counted in instances reports no weight, got {body}",
                row.case
            );
        }
    }
}

/// One row of the out-of-band termination table: how many instances the group holds and
/// how many of them are terminated in Amazon EC2 behind its back.
struct KilledRow {
    case: &'static str,
    capacity: i32,
    killed: usize,
}

/// An instance the account's Amazon EC2 store no longer runs is taken out of its Auto
/// Scaling group and replaced, so the group goes on holding the capacity it is configured
/// for — with one activity recording why the instance left and another why a new one was
/// launched.
#[tokio::test]
async fn an_instance_amazon_ec2_no_longer_runs_is_taken_out_of_the_group_and_replaced() {
    let rows = [
        KilledRow {
            case: "the only instance of the group",
            capacity: 1,
            killed: 1,
        },
        KilledRow {
            case: "one of three",
            capacity: 3,
            killed: 1,
        },
        KilledRow {
            case: "every instance of the group at once",
            capacity: 2,
            killed: 2,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let (router, engine) = emulator();
        let subnets = seed_subnets(&engine, 2);
        let template_name = format!("lt-killed-{index}");
        create_launch_template(&router, &template_name, "ami-00000000000000051", "t3.micro").await;

        let name = format!("asg-killed-{index}");
        let capacity = row.capacity.to_string();
        let identifier = format!("{},{}", subnets[0].id, subnets[1].id);
        let (status, body) = query(
            &router,
            "CreateAutoScalingGroup",
            &[
                ("AutoScalingGroupName", name.as_str()),
                ("LaunchTemplate.LaunchTemplateName", template_name.as_str()),
                ("MinSize", "0"),
                ("MaxSize", capacity.as_str()),
                ("DesiredCapacity", capacity.as_str()),
                ("VPCZoneIdentifier", identifier.as_str()),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: CreateAutoScalingGroup expected 200, got {status}: {body}",
            row.case
        );
        let wanted = usize::try_from(row.capacity).expect("a capacity is a count");
        let before = settled_instances(&router, &name, wanted).await;
        assert_eq!(
            before.len(),
            wanted,
            "case {:?}: the group must hold the capacity it was created with, got {before:?}",
            row.case
        );

        // Terminated OUTSIDE Auto Scaling: a caller of Amazon EC2's own API, which the
        // group never hears about.
        let killed: Vec<String> = before
            .iter()
            .take(row.killed)
            .map(|(instance_id, _, _)| instance_id.clone())
            .collect();
        let mut params: Vec<(String, String)> = Vec::new();
        for (position, instance_id) in killed.iter().enumerate() {
            params.push(p(&format!("InstanceId.{}", position + 1), instance_id.clone()));
        }
        let (status, body) = ec2_query(&router, "TerminateInstances", &params).await;
        assert_eq!(
            status, 200,
            "case {:?}: TerminateInstances expected 200, got {status}: {body}",
            row.case
        );

        // The group converges again: the killed instances are gone from it and fresh ones
        // stand in their place.
        let after = settled_instances(&router, &name, wanted).await;
        assert_eq!(
            after.len(),
            wanted,
            "case {:?}: the group must hold its capacity again, got {after:?}",
            row.case
        );
        for (instance_id, state, _) in &after {
            assert!(
                !killed.contains(instance_id),
                "case {:?}: {instance_id} was terminated in Amazon EC2, so the group must not \
                 still hold it: {after:?}",
                row.case
            );
            assert_eq!(
                state, "InService",
                "case {:?}: every member of the settled group is in service, got {after:?}",
                row.case
            );
            let described = described_instance(&router, instance_id)
                .await
                .unwrap_or_else(|| {
                    panic!(
                        "case {:?}: the replacement {instance_id} resolves to no Amazon EC2 \
                         instance",
                        row.case
                    )
                });
            assert_eq!(
                described.state, "running",
                "case {:?}: a replacement is an instance that runs, got {described:?}",
                row.case
            );
        }

        // What the activity log says happened, in the sentences Amazon EC2 Auto Scaling
        // words them with.
        let (status, body) = query(
            &router,
            "DescribeScalingActivities",
            &[("AutoScalingGroupName", name.as_str())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: DescribeScalingActivities expected 200, got {status}: {body}",
            row.case
        );
        let causes: Vec<String> = tag_values(&body, "Cause")
            .iter()
            .map(|cause| xml_unescape(cause))
            .collect();
        let taken_out = causes
            .iter()
            .filter(|cause| cause.contains(autoscaling::state::EC2_INSTANCE_GONE_REASON))
            .count();
        assert_eq!(
            taken_out,
            row.killed,
            "case {:?}: one activity per instance the health check took out, got {causes:?}",
            row.case
        );
        let replaced = causes
            .iter()
            .filter(|cause| cause.contains(autoscaling::state::UNHEALTHY_REPLACEMENT_REASON))
            .count();
        assert_eq!(
            replaced,
            row.killed,
            "case {:?}: one activity per replacement launched, got {causes:?}",
            row.case
        );
    }
}

/// One row of the refusal table: the group's name and how much capacity it holds —
/// neither of which may reach the refusal's message.
struct RefusalCase {
    case: &'static str,
    name: &'static str,
    capacity: &'static str,
}

/// `DeleteAutoScalingGroup` without `ForceDelete` on a group that still holds instances
/// is refused with the sentence Amazon EC2 Auto Scaling states for that error class,
/// which has NO variable part: it names neither the group nor any instance of it.
#[tokio::test]
async fn the_resource_in_use_refusal_is_the_fixed_sentence() {
    const EXPECTED: &str = "You cannot delete an AutoScalingGroup while there are instances or \
                            pending Spot instance request(s) still in the group.";

    let cases = [
        RefusalCase {
            case: "one instance in the group",
            name: "in-use-one",
            capacity: "1",
        },
        RefusalCase {
            case: "three instances in the group",
            name: "in-use-three",
            capacity: "3",
        },
        RefusalCase {
            case: "a group whose name would be a tempting suffix",
            name: "still.in-the-group",
            capacity: "2",
        },
    ];

    for case in &cases {
        let (router, engine) = emulator();
        let subnets = seed_subnets(&engine, 1);
        create_launch_template(&router, "lt-in-use", "ami-00000000000000031", "t3.micro").await;

        let (status, body) = query(
            &router,
            "CreateAutoScalingGroup",
            &[
                ("AutoScalingGroupName", case.name),
                ("LaunchTemplate.LaunchTemplateName", "lt-in-use"),
                ("MinSize", "0"),
                ("MaxSize", case.capacity),
                ("DesiredCapacity", case.capacity),
                ("VPCZoneIdentifier", subnets[0].id.as_str()),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: CreateAutoScalingGroup expected 200, got {status}: {body}",
            case.case
        );
        past_the_transitions().await;

        let (status, body) = query(
            &router,
            "DeleteAutoScalingGroup",
            &[("AutoScalingGroupName", case.name)],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {:?}: expected 400, got {status}: {body}",
            case.case
        );
        assert!(
            body.contains("<Code>ResourceInUse</Code>"),
            "case {:?}: expected the ResourceInUse query error code, got {body}",
            case.case
        );
        assert_eq!(
            tag_value(&body, "Message"),
            EXPECTED,
            "case {:?}: expected the fixed ResourceInUse sentence, got {body}",
            case.case
        );
        assert!(
            !tag_value(&body, "Message").contains(case.name),
            "case {:?}: the message must not name the group, got {body}",
            case.case
        );

        // The refusal left the group and its instances alone.
        let (_, body) = query(
            &router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", case.name)],
        )
        .await;
        assert_eq!(
            tag_values(&body, "InstanceId").len(),
            case.capacity
                .parse::<usize>()
                .expect("the capacity is a count"),
            "case {:?}: a refused delete leaves the group's instances alone, got {body}",
            case.case
        );
    }
}

/// How many times a case looks again to see that a warmup is still holding a refresh.
///
/// The warmup those cases run under is minutes long, so a refresh still at its one
/// replacement after this many looks is being held by it rather than racing the host.
const LOOKS_WHILE_HELD: u32 = 20;

/// One row of the unsatisfiable-requirements table: what the override's requirements ask
/// for that no offered instance type provides.
struct UnsatisfiableRow {
    case: &'static str,
    /// The `InstanceRequirements$AllowedInstanceTypes` patterns the override states.
    allowed_instance_types: &'static [&'static str],
    /// The `InstanceRequirements$VCpuCount` floor it states.
    minimum_vcpus: i32,
}

/// A group whose launch source chooses its instance type by ATTRIBUTES, where no instance
/// type offered to the account provides them, launches nothing and records why: there is no
/// type it may launch, so it does not fall back to one its requirements exclude.
#[tokio::test]
async fn a_launch_source_whose_requirements_no_instance_type_satisfies_launches_nothing() {
    let rows = [
        UnsatisfiableRow {
            case: "an allowed-type pattern no offered family matches",
            allowed_instance_types: &["zzz9.*"],
            minimum_vcpus: 2,
        },
        UnsatisfiableRow {
            case: "a vCPU floor above every offered type",
            allowed_instance_types: &[],
            minimum_vcpus: 1_000_000,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let (router, engine) = emulator();
        let subnets = seed_subnets(&engine, 1);
        let template_name = format!("lt-unsatisfiable-{index}");
        // The template names a type of its own, which the requirements exclude: a launch
        // that fell back to it would place an instance the group may not hold.
        create_launch_template(&router, &template_name, "ami-00000000000000071", "t3.micro").await;

        let name = format!("asg-unsatisfiable-{index}");
        let mut params: Vec<(String, String)> = vec![
            p("AutoScalingGroupName", name.clone()),
            p("MinSize", "0"),
            p("MaxSize", "2"),
            p("DesiredCapacity", "2"),
            p("VPCZoneIdentifier", subnets[0].id.clone()),
            p(
                "MixedInstancesPolicy.LaunchTemplate.LaunchTemplateSpecification.LaunchTemplateName",
                template_name.clone(),
            ),
            p(
                "MixedInstancesPolicy.LaunchTemplate.Overrides.member.1.InstanceRequirements.VCpuCount.Min",
                row.minimum_vcpus.to_string(),
            ),
            p(
                "MixedInstancesPolicy.LaunchTemplate.Overrides.member.1.InstanceRequirements.MemoryMiB.Min",
                "1024",
            ),
        ];
        for (position, pattern) in row.allowed_instance_types.iter().enumerate() {
            let member = position + 1;
            params.push(p(
                &format!(
                    "MixedInstancesPolicy.LaunchTemplate.Overrides.member.1.InstanceRequirements.\
                     AllowedInstanceTypes.member.{member}"
                ),
                *pattern,
            ));
        }
        let borrowed: Vec<(&str, &str)> = params
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed).await;
        assert_eq!(
            status, 200,
            "case {:?}: CreateAutoScalingGroup expected 200, got {status}: {body}",
            row.case
        );

        // The group exists and holds the capacity it was asked for, with no member at all:
        // the launch could not be made.
        let reported = reported_instances(&router, &name).await;
        assert!(
            reported.is_empty(),
            "case {:?}: a launch source with no type it may launch places nothing, got \
             {reported:?}",
            row.case
        );

        // And the activity log says why, in the sentence every launch failure of this
        // service ends with.
        let (status, body) = query(
            &router,
            "DescribeScalingActivities",
            &[("AutoScalingGroupName", name.as_str())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: DescribeScalingActivities expected 200, got {status}: {body}",
            row.case
        );
        let messages: Vec<String> = tag_values(&body, "StatusMessage")
            .iter()
            .map(|message| xml_unescape(message))
            .collect();
        let expected =
            autoscaling::state::ec2_instances::unsatisfiable_requirements_message();
        assert!(
            messages.contains(&expected),
            "case {:?}: expected an activity reporting {expected:?}, got {messages:?}",
            row.case
        );
    }
}

/// One row of the warmup table: where the warmup the refresh runs under comes from, and
/// what it comes to.
struct WarmupRow {    case: &'static str,
    /// `Preferences.InstanceWarmup` on the request, when it states one.
    requested_warmup: Option<i32>,
    /// The group's `DefaultInstanceWarmup`, when it has one.
    default_instance_warmup: Option<i32>,
    /// The group's `HealthCheckGracePeriod`, when it has one.
    health_check_grace_period: Option<i32>,
    /// The warmup in force, in seconds.
    effective_warmup: i32,
}

/// What `DescribeInstanceRefreshes` reports for a group's newest refresh: its status, what
/// it has still to update, the warmup it runs under and when it ended.
#[derive(Debug, Clone, Default)]
struct ReportedRefresh {
    status: String,
    instances_to_update: String,
    percentage_complete: String,
    instance_warmup: String,
    end_time: String,
}

/// Describe one group's refreshes and read the newest one out of the answer.
async fn newest_refresh(router: &MultiServiceRouter, name: &str) -> ReportedRefresh {
    let (status, body) = query(
        router,
        "DescribeInstanceRefreshes",
        &[("AutoScalingGroupName", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeInstanceRefreshes for {name} expected 200, got {status}: {body}"
    );
    ReportedRefresh {
        status: tag_value(&body, "Status"),
        instances_to_update: tag_value(&body, "InstancesToUpdate"),
        percentage_complete: tag_value(&body, "PercentageComplete"),
        instance_warmup: tag_value(&body, "InstanceWarmup"),
        end_time: tag_value(&body, "EndTime"),
    }
}

/// An instance refresh waits the warmup it runs under after a replacement comes into
/// service before it moves on, and is reported `InProgress` for as long as it waits — so a
/// client polling a running refresh sees it running, and the group holds its capacity
/// throughout.
///
/// The table spans where that warmup comes from: the request's own
/// `Preferences.InstanceWarmup`, the group's `DefaultInstanceWarmup` and its
/// `HealthCheckGracePeriod` — the fallback chain that member documents — and the two ways it
/// comes to nothing, where the refresh runs to completion instead.
#[tokio::test]
async fn a_refresh_waits_the_warmup_it_runs_under_after_each_replacement() {
    // Minutes, so that a refresh held by the warmup is still held however slowly the host
    // runs this case.
    const LONG_WARMUP: i32 = 600;

    let rows = [
        WarmupRow {
            case: "the request's own InstanceWarmup",
            requested_warmup: Some(LONG_WARMUP),
            default_instance_warmup: None,
            health_check_grace_period: None,
            effective_warmup: LONG_WARMUP,
        },
        WarmupRow {
            case: "the group's DefaultInstanceWarmup, which the request leaves out",
            requested_warmup: None,
            default_instance_warmup: Some(LONG_WARMUP),
            health_check_grace_period: None,
            effective_warmup: LONG_WARMUP,
        },
        WarmupRow {
            case: "the group's HealthCheckGracePeriod, the last of the documented fallbacks",
            requested_warmup: None,
            default_instance_warmup: None,
            health_check_grace_period: Some(LONG_WARMUP),
            effective_warmup: LONG_WARMUP,
        },
        WarmupRow {
            case: "no warmup anywhere: the refresh waits nothing of its own",
            requested_warmup: None,
            default_instance_warmup: None,
            health_check_grace_period: None,
            effective_warmup: 0,
        },
        WarmupRow {
            case: "a request that states none at all overrides the group's own",
            requested_warmup: Some(0),
            default_instance_warmup: Some(LONG_WARMUP),
            health_check_grace_period: None,
            effective_warmup: 0,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let (router, engine) = emulator();
        let subnets = seed_subnets(&engine, 1);
        let template_name = format!("lt-warmup-{index}");
        create_launch_template(&router, &template_name, "ami-00000000000000061", "t3.micro").await;

        // Two instances, so a refresh held after its first replacement still has one to
        // make.
        let name = format!("asg-warmup-{index}");
        let default_warmup = row.default_instance_warmup.map(|value| value.to_string());
        let grace_period = row.health_check_grace_period.map(|value| value.to_string());
        let mut params: Vec<(&str, &str)> = vec![
            ("AutoScalingGroupName", name.as_str()),
            ("LaunchTemplate.LaunchTemplateName", template_name.as_str()),
            ("MinSize", "0"),
            ("MaxSize", "2"),
            ("DesiredCapacity", "2"),
            ("VPCZoneIdentifier", subnets[0].id.as_str()),
        ];
        if let Some(value) = default_warmup.as_deref() {
            params.push(("DefaultInstanceWarmup", value));
        }
        if let Some(value) = grace_period.as_deref() {
            params.push(("HealthCheckGracePeriod", value));
        }
        let (status, body) = query(&router, "CreateAutoScalingGroup", &params).await;
        assert_eq!(
            status, 200,
            "case {:?}: CreateAutoScalingGroup expected 200, got {status}: {body}",
            row.case
        );
        let before: Vec<String> = settled_instances(&router, &name, 2)
            .await
            .into_iter()
            .map(|(instance_id, _, _)| instance_id)
            .collect();
        assert_eq!(
            before.len(),
            2,
            "case {:?}: the group must hold the capacity it was created with, got {before:?}",
            row.case
        );

        let requested_warmup = row.requested_warmup.map(|value| value.to_string());
        let mut params: Vec<(&str, &str)> = vec![("AutoScalingGroupName", name.as_str())];
        if let Some(value) = requested_warmup.as_deref() {
            params.push(("Preferences.InstanceWarmup", value));
        }
        let (status, body) = query(&router, "StartInstanceRefresh", &params).await;
        assert_eq!(
            status, 200,
            "case {:?}: StartInstanceRefresh expected 200, got {status}: {body}",
            row.case
        );

        // The warmup the refresh RUNS UNDER, which a client reads back off the refresh
        // itself whichever member put it in force.
        let reported = newest_refresh(&router, &name).await;
        assert_eq!(
            reported.instance_warmup,
            row.effective_warmup.to_string(),
            "case {:?}: the refresh reports the warmup in force, got {reported:?}",
            row.case
        );

        if row.effective_warmup == 0 {
            // Nothing holds it, so it runs to the end: every instance replaced.
            let deadline = tokio::time::Instant::now() + settle_deadline();
            let reported = loop {
                let reported = newest_refresh(&router, &name).await;
                if reported.status == "Successful" {
                    break reported;
                }
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "case {:?}: a refresh with no warmup must finish, last saw {reported:?}",
                    row.case
                );
                tokio::time::sleep(LOOK_AGAIN).await;
            };
            assert_eq!(
                reported.instances_to_update, "0",
                "case {:?}: a finished refresh has nothing left to update, got {reported:?}",
                row.case
            );
            assert_eq!(
                reported.percentage_complete, "100",
                "case {:?}: a finished refresh is complete, got {reported:?}",
                row.case
            );
            let after = settled_instances(&router, &name, 2).await;
            assert!(
                after
                    .iter()
                    .all(|(instance_id, _, _)| !before.contains(instance_id)),
                "case {:?}: a finished refresh replaced every instance, got {after:?} for \
                 {before:?}",
                row.case
            );
            continue;
        }

        // The refresh makes its first replacement and then waits: look again until that
        // replacement has been made.
        let deadline = tokio::time::Instant::now() + settle_deadline();
        let reported = loop {
            let reported = newest_refresh(&router, &name).await;
            if reported.instances_to_update == "1" {
                break reported;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "case {:?}: the refresh must replace its first instance, last saw {reported:?}",
                row.case
            );
            tokio::time::sleep(LOOK_AGAIN).await;
        };
        assert_eq!(
            reported.status, "InProgress",
            "case {:?}: a refresh that is still replacing instances is reported InProgress, \
             got {reported:?}",
            row.case
        );
        assert!(
            reported.end_time.is_empty(),
            "case {:?}: a running refresh has not ended, got {reported:?}",
            row.case
        );

        // And it is STILL waiting on every later look: the warmup in force is minutes long,
        // so nothing of it has elapsed.
        for look in 0..LOOKS_WHILE_HELD {
            let reported = newest_refresh(&router, &name).await;
            assert_eq!(
                reported.status, "InProgress",
                "case {:?}: look {look}: the warmup holds the refresh InProgress, got \
                 {reported:?}",
                row.case
            );
            assert_eq!(
                reported.instances_to_update, "1",
                "case {:?}: look {look}: the refresh moves on only once the warmup has \
                 elapsed, got {reported:?}",
                row.case
            );
            tokio::time::sleep(LOOK_AGAIN).await;
        }

        // The replacement it did make is an instance of the group, in place of the one it
        // replaced: the group holds its capacity throughout.
        let during = settled_instances(&router, &name, 2).await;
        assert_eq!(
            during.len(),
            2,
            "case {:?}: the group holds its capacity while the refresh runs, got {during:?}",
            row.case
        );
        assert_eq!(
            during
                .iter()
                .filter(|(instance_id, _, _)| before.contains(instance_id))
                .count(),
            1,
            "case {:?}: exactly one of the group's two instances has been replaced, got \
             {during:?} for {before:?}",
            row.case
        );
    }
}
