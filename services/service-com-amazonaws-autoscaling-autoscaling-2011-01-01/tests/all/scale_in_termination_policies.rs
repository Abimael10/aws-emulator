// SPDX-License-Identifier: CC0-1.0

//! Which instances a SCALE-IN of an Auto Scaling group terminates.
//!
//! A capacity decrease names no instance, so the group's own `TerminationPolicies`
//! select the ones that leave: "A policy or a list of policies that are used to select
//! the instances to terminate. The policies are executed in the order that you list
//! them"
//! (`smithy_query(query="doc",shape="UpdateAutoScalingGroupType",member="TerminationPolicies")`).
//! Lowering the capacity of a group that holds more than the new capacity therefore
//! terminates the instances those policies pick — in the account's own Amazon EC2 store,
//! where the group's instances live — and the group stops reporting them.
//!
//! The table below spans what that rule generalises over: which policy the group names
//! and whether it names one or several (including lists whose later policy disagrees
//! with the launch order, and lists that repeat a policy or interleave ones this
//! emulator does not model), whether it names the policy at the create or at the update
//! that scales in, how many instances the group holds, how far it is scaled in (one
//! instance, several, all of them), whether its instances were launched together or over
//! two capacity changes, and whether the decrease is asked for by
//! `UpdateAutoScalingGroup` or by `SetDesiredCapacity`. The policies this emulator does
//! not model are rows too: for those, how MANY instances leave is the whole rule.
//!
//! Every wait here looks again until the group has settled, never for a span: the
//! lifecycle transitions are owed on a clock, so a fixed sleep would be a guess about
//! how fast the host runs.
//!
//! The suite's second subject is the other half of that same seam — HOW MANY instances a
//! capacity change moves. A group whose mixed instances policy weights its instance types
//! states its three size parameters in those weights' units, so the capacity asked for is
//! fulfilled by the instances whose weights add up to it and each member reports the units
//! it contributes; the tables at the end of this file span that rule.

use std::sync::Arc;
use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;
use service_com_amazonaws_ec2_amazonec2 as ec2_service;
use service_com_amazonaws_ec2_amazonec2::state as ec2;

use crate::support::{borrowed, ec2_query, p, query, tag_value, tag_values, xml_unescape, REGION};

/// The launch source every group here names. The template need not exist in Amazon EC2:
/// what these cases are about is WHICH of the group's instances a scale-in picks, not
/// what they run.
const LAUNCH_TEMPLATE_NAME: &str = "lt-termination-policies";

/// The `LifecycleState` of an instance that is in service, spelled as the model's own
/// `com.amazonaws.autoscaling#LifecycleState` enum spells it
/// (`smithy_query(query="shape",shape="LifecycleState")`).
const IN_SERVICE: &str = "InService";

/// How long a wait for the group to settle may take before the test gives up. Generous
/// on purpose: the condition is what is waited for, and a loaded host may need many
/// passes to reach it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long to wait between two looks while waiting for a condition.
const LOOK_AGAIN: Duration = Duration::from_millis(20);

/// One emulator with Auto Scaling and Amazon EC2 on ONE engine: the instances a group
/// launches and terminates are the account's own EC2 instances, so one engine is what
/// makes the two views one account.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    // A group launches from a launch template the account holds, so the one this suite's
    // groups name is put in the account's own Amazon EC2 store first.
    crate::support::seed_launch_templates(&engine, &[LAUNCH_TEMPLATE_NAME]);
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(autoscaling::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(ec2_service::EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// The zone every group here places its instances in: the first of the Region's own
/// catalog, never written out, and ONE zone so that no Availability Zone balancing can
/// explain which instance a policy picked.
fn only_zone() -> String {
    ec2::availability_zones_in_region(REGION)
        .first()
        .map(|zone| zone.zone_name.clone())
        .expect("the Region has Availability Zones")
}

/// How the capacity change that scales the group in is asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AskedBy {
    /// `UpdateAutoScalingGroup$DesiredCapacity`.
    Update,
    /// `SetDesiredCapacity$DesiredCapacity`.
    SetDesiredCapacity,
}

/// Where the group's `TerminationPolicies` come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PoliciesFrom {
    /// `CreateAutoScalingGroup$TerminationPolicies`.
    Create,
    /// The same `UpdateAutoScalingGroup` that lowers the capacity.
    TheUpdateThatScalesIn,
}

/// One row of the scale-in table.
struct Row {
    case: &'static str,
    /// The group's `TerminationPolicies`, in the order it lists them; empty names none,
    /// which leaves the group with the service's own default policy.
    policies: &'static [&'static str],
    policies_from: PoliciesFrom,
    /// The capacity the create asks for, launching the group's first instances.
    created: i32,
    /// A capacity the group is grown to before being scaled in, when the row grows it:
    /// its instances are then launched over two capacity changes rather than together.
    grown_to: Option<i32>,
    /// The capacity the group is then scaled in to.
    scaled_to: i32,
    asked_by: AskedBy,
    /// The positions IN LAUNCH ORDER of the instances the scale-in must take out, or
    /// `None` for a group whose policies name nothing this emulator models — there the
    /// rule is only that the surplus leaves.
    expected_gone: Option<&'static [usize]>,
}

/// Create one group over the awsQuery wire.
async fn create_group(router: &MultiServiceRouter, name: &str, row: &Row) {
    let largest = row.created.max(row.grown_to.unwrap_or(0));
    let mut params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", LAUNCH_TEMPLATE_NAME),
        p("MinSize", "0"),
        p("MaxSize", largest.to_string()),
        p("DesiredCapacity", row.created.to_string()),
        p("AvailabilityZones.member.1", only_zone()),
    ];
    if row.policies_from == PoliciesFrom::Create {
        for (index, policy) in row.policies.iter().enumerate() {
            params.push(p(
                format!("TerminationPolicies.member.{}", index + 1).as_str(),
                *policy,
            ));
        }
    }
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {:?}: CreateAutoScalingGroup got {status}: {body}",
        row.case
    );
}

/// Ask for `capacity` on the group, the way `row` says a capacity change is asked for,
/// carrying the group's `TerminationPolicies` when the row names them here.
async fn ask_for_capacity(
    router: &MultiServiceRouter,
    name: &str,
    row: &Row,
    capacity: i32,
    carry_policies: bool,
) {
    let mut params = vec![
        p("AutoScalingGroupName", name),
        p("DesiredCapacity", capacity.to_string()),
    ];
    let action = match row.asked_by {
        AskedBy::Update => "UpdateAutoScalingGroup",
        AskedBy::SetDesiredCapacity => "SetDesiredCapacity",
    };
    if carry_policies && row.policies_from == PoliciesFrom::TheUpdateThatScalesIn {
        for (index, policy) in row.policies.iter().enumerate() {
            params.push(p(
                format!("TerminationPolicies.member.{}", index + 1).as_str(),
                *policy,
            ));
        }
    }
    let (status, body) = query(router, action, &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {:?}: {action} to {capacity} got {status}: {body}",
        row.case
    );
}

/// Every instance the group reports, as `(instance id, LifecycleState)` in the order it
/// reports them — which is the order the group launched them in.
async fn reported_instances(router: &MultiServiceRouter, name: &str) -> Vec<(String, String)> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeAutoScalingGroups for {name} got {status}: {body}"
    );
    let ids = tag_values(&body, "InstanceId");
    let states = tag_values(&body, "LifecycleState");
    assert_eq!(
        ids.len(),
        states.len(),
        "every reported instance has a LifecycleState, got {body}"
    );
    ids.into_iter().zip(states).collect()
}

/// Look again until the group holds exactly `expected` instances and every one of them
/// is in service, and answer their ids in launch order.
///
/// The transitions a capacity change starts are owed on a clock, so this waits for the
/// CONDITION: it polls until the group has settled or [`SETTLE_DEADLINE`] passes, and
/// never sleeps for a span it then assumes was enough.
async fn settled_instances(
    router: &MultiServiceRouter,
    name: &str,
    expected: usize,
    case: &str,
) -> Vec<String> {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let reported = reported_instances(router, name).await;
        if reported.len() == expected && reported.iter().all(|(_, state)| state == IN_SERVICE) {
            return reported.into_iter().map(|(id, _)| id).collect();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the group to settle at {expected} instances in service, \
             got {reported:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The group's `TerminationPolicies` select the instances a scale-in terminates.
#[tokio::test]
async fn a_scale_in_terminates_the_instances_the_termination_policies_select() {
    let rows = [
        Row {
            case: "the oldest instance of a group grown one instance at a time",
            policies: &["OldestInstance"],
            policies_from: PoliciesFrom::Create,
            created: 1,
            grown_to: Some(2),
            scaled_to: 1,
            asked_by: AskedBy::Update,
            expected_gone: Some(&[0]),
        },
        Row {
            case: "the oldest of three instances launched together",
            policies: &["OldestInstance"],
            policies_from: PoliciesFrom::Create,
            created: 3,
            grown_to: None,
            scaled_to: 2,
            asked_by: AskedBy::SetDesiredCapacity,
            expected_gone: Some(&[0]),
        },
        Row {
            case: "the two oldest of four, launched over two capacity changes",
            policies: &["OldestInstance"],
            policies_from: PoliciesFrom::Create,
            created: 2,
            grown_to: Some(4),
            scaled_to: 2,
            asked_by: AskedBy::Update,
            expected_gone: Some(&[0, 1]),
        },
        Row {
            case: "the newest instance, the mirror of the oldest",
            policies: &["NewestInstance"],
            policies_from: PoliciesFrom::Create,
            created: 1,
            grown_to: Some(3),
            scaled_to: 2,
            asked_by: AskedBy::SetDesiredCapacity,
            expected_gone: Some(&[2]),
        },
        Row {
            case: "the two newest of four launched together",
            policies: &["NewestInstance"],
            policies_from: PoliciesFrom::Create,
            created: 4,
            grown_to: None,
            scaled_to: 2,
            asked_by: AskedBy::Update,
            expected_gone: Some(&[2, 3]),
        },
        Row {
            case: "two policies in order: the first tells none of them apart, so the \
                   second decides",
            policies: &["OldestLaunchTemplate", "OldestInstance"],
            policies_from: PoliciesFrom::Create,
            created: 3,
            grown_to: None,
            scaled_to: 1,
            asked_by: AskedBy::Update,
            expected_gone: Some(&[0, 1]),
        },
        Row {
            case: "two policies in order whose second DISAGREES with the launch order: \
                   the first tells none of them apart, so the newest goes",
            policies: &["OldestLaunchTemplate", "NewestInstance"],
            policies_from: PoliciesFrom::Create,
            created: 3,
            grown_to: None,
            scaled_to: 2,
            asked_by: AskedBy::Update,
            expected_gone: Some(&[2]),
        },
        Row {
            case: "a first policy every instance of one usage hour ties on, so the \
                   second decides against the launch order",
            policies: &["ClosestToNextInstanceHour", "NewestInstance"],
            policies_from: PoliciesFrom::Create,
            created: 3,
            grown_to: None,
            scaled_to: 1,
            asked_by: AskedBy::SetDesiredCapacity,
            expected_gone: Some(&[1, 2]),
        },
        Row {
            case: "a list that repeats a policy and interleaves ones that order \
                   nothing: the repeat and the no-ops decide nothing",
            policies: &[
                "Default",
                "NewestInstance",
                "AllocationStrategy",
                "NewestInstance",
                "OldestInstance",
            ],
            policies_from: PoliciesFrom::Create,
            created: 3,
            grown_to: None,
            scaled_to: 2,
            asked_by: AskedBy::Update,
            expected_gone: Some(&[2]),
        },
        Row {
            case: "the policy named by the same update that scales the group in",
            policies: &["OldestInstance"],
            policies_from: PoliciesFrom::TheUpdateThatScalesIn,
            created: 2,
            grown_to: None,
            scaled_to: 1,
            asked_by: AskedBy::Update,
            expected_gone: Some(&[0]),
        },
        Row {
            case: "scaling all the way in takes every instance the group holds",
            policies: &["OldestInstance"],
            policies_from: PoliciesFrom::Create,
            created: 2,
            grown_to: None,
            scaled_to: 0,
            asked_by: AskedBy::SetDesiredCapacity,
            expected_gone: Some(&[0, 1]),
        },
        Row {
            case: "closest to the next instance hour: every instance of the group is in \
                   the same usage hour, so only the surplus leaving is the rule",
            policies: &["ClosestToNextInstanceHour"],
            policies_from: PoliciesFrom::Create,
            created: 3,
            grown_to: None,
            scaled_to: 2,
            asked_by: AskedBy::Update,
            expected_gone: None,
        },
        Row {
            case: "the allocation strategy of a mixed instances policy, which this \
                   group does not have",
            policies: &["AllocationStrategy"],
            policies_from: PoliciesFrom::Create,
            created: 2,
            grown_to: None,
            scaled_to: 1,
            asked_by: AskedBy::SetDesiredCapacity,
            expected_gone: None,
        },
        Row {
            case: "a group that names no policy at all",
            policies: &[],
            policies_from: PoliciesFrom::Create,
            created: 2,
            grown_to: None,
            scaled_to: 1,
            asked_by: AskedBy::Update,
            expected_gone: None,
        },
        Row {
            case: "a custom policy, named by the Lambda function ARN form",
            policies: &["arn:aws:lambda:us-west-2:123456789012:function:pick:live"],
            policies_from: PoliciesFrom::Create,
            created: 3,
            grown_to: None,
            scaled_to: 1,
            asked_by: AskedBy::Update,
            expected_gone: None,
        },
    ];

    // Every policy a row names is one of the values the service accepts, from the
    // model's own documentation for `UpdateAutoScalingGroupType$TerminationPolicies`
    // ("Valid values: Default | AllocationStrategy | ClosestToNextInstanceHour |
    // NewestInstance | OldestInstance | OldestLaunchConfiguration |
    // OldestLaunchTemplate | arn:aws:lambda:region:account-id:function:my-function:my-alias"),
    // read through the crate's own catalog so the two cannot drift apart — so no row
    // passes on a policy the group would have refused.
    for row in &rows {
        for policy in row.policies {
            assert!(
                autoscaling::state::TERMINATION_POLICIES.contains(policy)
                    || policy.starts_with(autoscaling::state::TERMINATION_POLICY_LAMBDA_PREFIX),
                "case {:?}: {policy} must be a policy a group may name",
                row.case
            );
        }
    }

    for (index, row) in rows.iter().enumerate() {
        // One emulator per row: each row is its own account, so the ids of one row
        // never explain another's.
        let (router, _engine) = emulator();
        let name = format!("asg-scale-in-{index}");
        create_group(&router, &name, row).await;
        let mut launched = settled_instances(
            &router,
            &name,
            usize::try_from(row.created).expect("a capacity is a count"),
            row.case,
        )
        .await;

        if let Some(grown_to) = row.grown_to {
            ask_for_capacity(&router, &name, row, grown_to, false).await;
            launched = settled_instances(
                &router,
                &name,
                usize::try_from(grown_to).expect("a capacity is a count"),
                row.case,
            )
            .await;
        }

        ask_for_capacity(&router, &name, row, row.scaled_to, true).await;
        let left = settled_instances(
            &router,
            &name,
            usize::try_from(row.scaled_to).expect("a capacity is a count"),
            row.case,
        )
        .await;

        match row.expected_gone {
            Some(gone) => {
                let expected: Vec<String> = launched
                    .iter()
                    .enumerate()
                    .filter(|(position, _)| !gone.contains(position))
                    .map(|(_, id)| id.clone())
                    .collect();
                assert_eq!(
                    left, expected,
                    "case {:?}: the scale-in must take the instances at {gone:?} of the \
                     launch order {launched:?}, got {left:?}",
                    row.case
                );
            }
            None => {
                assert!(
                    left.iter().all(|id| launched.contains(id)),
                    "case {:?}: every instance left is one the group launched {launched:?}, \
                     got {left:?}",
                    row.case
                );
            }
        }
    }
}

/// The ids the group stops reporting are terminated in the account's own Amazon EC2
/// store, and the ones it keeps are still running: a scale-in that picked the oldest
/// instance shuts down THAT instance where it lives.
#[tokio::test]
async fn the_instance_a_scale_in_picked_is_terminated_in_amazon_ec2() {
    let (router, _engine) = emulator();
    let name = "asg-ec2-scale-in";
    let row = Row {
        case: "the oldest instance of a group of two",
        policies: &["OldestInstance"],
        policies_from: PoliciesFrom::Create,
        created: 1,
        grown_to: Some(2),
        scaled_to: 1,
        asked_by: AskedBy::Update,
        expected_gone: Some(&[0]),
    };
    create_group(&router, name, &row).await;
    let first = settled_instances(&router, name, 1, row.case).await;
    ask_for_capacity(&router, name, &row, 2, false).await;
    let both = settled_instances(&router, name, 2, row.case).await;
    assert_eq!(
        both.first(),
        first.first(),
        "the group keeps the instance it already held, got {both:?} after {first:?}"
    );
    let (oldest, newer) = (both[0].clone(), both[1].clone());

    ask_for_capacity(&router, name, &row, 1, true).await;

    wait_for_ec2_state(&router, &oldest, "terminated").await;
    assert_eq!(
        ec2_instance_state(&router, &newer).await,
        "running",
        "the instance the scale-in did not pick stays running, got {newer}"
    );

    let left = settled_instances(&router, name, 1, row.case).await;
    assert_eq!(
        left,
        vec![newer.clone()],
        "the group reports only the instance that survived the scale-in, got {left:?}"
    );
}

/// An instance protected from scale-in is not chosen however the group's policies order
/// it: the protection outranks the policy, and the policy then picks among the rest.
#[tokio::test]
async fn scale_in_protection_outranks_the_termination_policy() {
    let (router, _engine) = emulator();
    let name = "asg-protected-oldest";
    let row = Row {
        case: "the oldest instance protected from scale-in",
        policies: &["OldestInstance"],
        policies_from: PoliciesFrom::Create,
        created: 2,
        grown_to: None,
        scaled_to: 1,
        asked_by: AskedBy::SetDesiredCapacity,
        expected_gone: Some(&[1]),
    };
    create_group(&router, name, &row).await;
    let launched = settled_instances(&router, name, 2, row.case).await;
    let (oldest, newer) = (launched[0].clone(), launched[1].clone());

    let params = vec![
        p("AutoScalingGroupName", name),
        p("InstanceIds.member.1", oldest.clone()),
        p("ProtectedFromScaleIn", "true"),
    ];
    let (status, body) = query(&router, "SetInstanceProtection", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "SetInstanceProtection on {oldest} got {status}: {body}"
    );

    ask_for_capacity(&router, name, &row, 1, true).await;
    let left = settled_instances(&router, name, 1, row.case).await;
    assert_eq!(
        left,
        vec![oldest.clone()],
        "the protected oldest instance must be the one that stays, got {left:?} of \
         {launched:?}"
    );
    assert_eq!(
        ec2_instance_state(&router, &oldest).await,
        "running",
        "the protected instance is still running in Amazon EC2, got {oldest}"
    );
    // The instance the policy picked once the protected one was out of the running is
    // shut down where it lives, which is the state the scale-in owes it.
    wait_for_ec2_state(&router, &newer, "terminated").await;
}

/// What Amazon EC2 reports one instance's state as, read from a `DescribeInstances` that
/// named exactly that instance. `""` when EC2 resolves the id to nothing.
async fn ec2_instance_state(router: &MultiServiceRouter, instance_id: &str) -> String {
    let (status, body) =
        ec2_query(router, "DescribeInstances", &[p("InstanceId.1", instance_id)]).await;
    if status != 200 {
        return String::new();
    }
    tag_value(&tag_value(&body, "instanceState"), "name")
}

/// Look again until Amazon EC2 reports `instance_id` in `expected`.
///
/// Amazon EC2 takes an instance through `shutting-down` before `terminated`, on its own
/// clock, so this waits for the STATE rather than for a span: it polls until EC2 reports
/// it or [`SETTLE_DEADLINE`] passes.
async fn wait_for_ec2_state(router: &MultiServiceRouter, instance_id: &str, expected: &str) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let state = ec2_instance_state(router, instance_id).await;
        if state == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "expected Amazon EC2 to report {instance_id} {expected}, got {state:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

// ---------------------------------------------------------------------------------------
// A WEIGHTED group's size is stated in CAPACITY UNITS.
//
// "If you provide a list of instance types to use, you can specify the number of capacity
// units provided by each instance type … the capacity units count toward the desired
// capacity. Amazon EC2 Auto Scaling launches instances until the desired capacity is
// totally fulfilled, even if this results in an overage … Every Auto Scaling group has
// three size parameters (DesiredCapacity, MaxSize, and MinSize) … if you configure a mixed
// instances policy that defines weights for the instance types, you must specify these
// sizes with the same units that you use for weighting instances"
// (`smithy_query(query="shape",shape="LaunchTemplateOverrides")`, member
// `WeightedCapacity`). The weight each member contributes is reported on both read paths:
// `Instance$WeightedCapacity` and `AutoScalingInstanceDetails$WeightedCapacity`, "the
// number of capacity units contributed by the instance based on its instance type".
// ---------------------------------------------------------------------------------------

/// The instance type the weighted groups below override, so one override carries one
/// weight.
const WEIGHTED_INSTANCE_TYPE: &str = "t3.micro";

/// Where the capacity a weighted group is driven to is asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CapacityFrom {
    /// `CreateAutoScalingGroup$DesiredCapacity`.
    Create,
    /// A later `SetDesiredCapacity`.
    SetDesiredCapacity,
    /// A later `UpdateAutoScalingGroup`.
    Update,
}

/// One row of the weighted-capacity table.
struct WeightRow {
    case: &'static str,
    /// The `WeightedCapacity` the group's single override carries, or `None` for an
    /// override that states none.
    weight: Option<&'static str>,
    /// The group's `MaxSize`, in the same units as the capacity asked for.
    max_size: i32,
    /// The capacity asked for, in capacity units.
    units: i32,
    asked_by: CapacityFrom,
    /// How many instances fulfil that capacity: the fewest whose weights reach it.
    expected_instances: usize,
}

/// One member of a group, as `DescribeAutoScalingGroups` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedMember {
    instance_id: String,
    instance_type: String,
    lifecycle_state: String,
    /// `Instance$WeightedCapacity`, absent when the group states no weight for the type.
    weighted_capacity: Option<String>,
}

/// Create one group whose mixed instances policy overrides [`WEIGHTED_INSTANCE_TYPE`],
/// carrying `row`'s weight, and ask for the row's capacity the way the row says.
async fn create_weighted_group(router: &MultiServiceRouter, name: &str, row: &WeightRow) {
    let mut params = vec![
        p("AutoScalingGroupName", name),
        p(
            "MixedInstancesPolicy.LaunchTemplate.LaunchTemplateSpecification.LaunchTemplateName",
            LAUNCH_TEMPLATE_NAME,
        ),
        p(
            "MixedInstancesPolicy.LaunchTemplate.Overrides.member.1.InstanceType",
            WEIGHTED_INSTANCE_TYPE,
        ),
        p("MinSize", "0"),
        p("MaxSize", row.max_size.to_string()),
        p("AvailabilityZones.member.1", only_zone()),
    ];
    if let Some(weight) = row.weight {
        params.push(p(
            "MixedInstancesPolicy.LaunchTemplate.Overrides.member.1.WeightedCapacity",
            weight,
        ));
    }
    if row.asked_by == CapacityFrom::Create {
        params.push(p("DesiredCapacity", row.units.to_string()));
    }
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {:?}: CreateAutoScalingGroup expected 200, got {status}: {body}",
        row.case
    );
    let action = match row.asked_by {
        CapacityFrom::Create => return,
        CapacityFrom::SetDesiredCapacity => "SetDesiredCapacity",
        CapacityFrom::Update => "UpdateAutoScalingGroup",
    };
    let params = vec![
        p("AutoScalingGroupName", name),
        p("DesiredCapacity", row.units.to_string()),
    ];
    let (status, body) = query(router, action, &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {:?}: {action} to {} units expected 200, got {status}: {body}",
        row.case, row.units
    );
}

/// The members one group reports, read out of the `Instances` member of a
/// `DescribeAutoScalingGroups` answer — scoped to that element, so the weights the group's
/// own `MixedInstancesPolicy` echoes back are not read as a member's.
fn reported_members(body: &str) -> Vec<ReportedMember> {
    let instances = tag_value(body, "Instances");
    instances
        .split("</member>")
        .filter(|member| member.contains("<InstanceId"))
        .map(|member| ReportedMember {
            instance_id: tag_value(member, "InstanceId"),
            instance_type: tag_value(member, "InstanceType"),
            lifecycle_state: tag_value(member, "LifecycleState"),
            weighted_capacity: {
                let weight = tag_value(member, "WeightedCapacity");
                (!weight.is_empty()).then_some(weight)
            },
        })
        .collect()
}

/// Read one group, and answer its `DesiredCapacity` beside the members it reports.
async fn described_group(
    router: &MultiServiceRouter,
    name: &str,
) -> (String, Vec<ReportedMember>) {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeAutoScalingGroups for {name} got {status}: {body}"
    );
    (tag_value(&body, "DesiredCapacity"), reported_members(&body))
}

/// Look again until the group reports `expected` members and every one of them is in
/// service, and answer what it last reported.
///
/// The CONDITION, never a fixed wait: the launches and terminations a capacity change
/// starts are owed on a clock.
async fn settled_members(
    router: &MultiServiceRouter,
    name: &str,
    expected: usize,
    case: &str,
) -> Vec<ReportedMember> {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (_, members) = described_group(router, name).await;
        if members.len() == expected
            && members
                .iter()
                .all(|member| member.lifecycle_state == IN_SERVICE)
        {
            return members;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the group to settle at {expected} members in service, \
             got {members:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// What `DescribeAutoScalingInstances` reports for one instance: the group it belongs to
/// and the capacity units it contributes.
async fn described_instances(
    router: &MultiServiceRouter,
    instance_ids: &[String],
) -> Vec<(String, String, Option<String>)> {
    let params: Vec<(String, String)> = instance_ids
        .iter()
        .enumerate()
        .map(|(index, instance_id)| {
            p(
                &format!("InstanceIds.member.{}", index + 1),
                instance_id.as_str(),
            )
        })
        .collect();
    let (status, body) =
        query(router, "DescribeAutoScalingInstances", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "DescribeAutoScalingInstances for {instance_ids:?} got {status}: {body}"
    );
    tag_value(&body, "AutoScalingInstances")
        .split("</member>")
        .filter(|member| member.contains("<InstanceId"))
        .map(|member| {
            let weight = tag_value(member, "WeightedCapacity");
            (
                tag_value(member, "InstanceId"),
                tag_value(member, "AutoScalingGroupName"),
                (!weight.is_empty()).then_some(weight),
            )
        })
        .collect()
}

/// A weighted group's `DesiredCapacity` is a number of CAPACITY UNITS: the group launches
/// the fewest instances whose weights fulfil it, and every read of a member reports the
/// units that member contributes.
#[tokio::test]
async fn a_weighted_group_is_sized_in_capacity_units() {
    let rows = [
        WeightRow {
            case: "two units at a weight of two: ONE instance",
            weight: Some("2"),
            max_size: 4,
            units: 2,
            asked_by: CapacityFrom::SetDesiredCapacity,
            expected_instances: 1,
        },
        WeightRow {
            case: "three units at a weight of two: the launch overshoots to four units",
            weight: Some("2"),
            max_size: 4,
            units: 3,
            asked_by: CapacityFrom::SetDesiredCapacity,
            expected_instances: 2,
        },
        WeightRow {
            case: "six units at a weight of three: two instances, asked for by the create",
            weight: Some("3"),
            max_size: 6,
            units: 6,
            asked_by: CapacityFrom::Create,
            expected_instances: 2,
        },
        WeightRow {
            case: "the smallest published weight: a unit per instance",
            weight: Some("1"),
            max_size: 3,
            units: 3,
            asked_by: CapacityFrom::Update,
            expected_instances: 3,
        },
        WeightRow {
            case: "an override that states no weight: the size is an instance count",
            weight: None,
            max_size: 2,
            units: 2,
            asked_by: CapacityFrom::SetDesiredCapacity,
            expected_instances: 2,
        },
        WeightRow {
            case: "the largest published weight: one instance fulfils 999 units",
            weight: Some("999"),
            max_size: 999,
            units: 999,
            asked_by: CapacityFrom::SetDesiredCapacity,
            expected_instances: 1,
        },
        WeightRow {
            case: "one past the published maximum: counted as the override states it, and \
                   reported as it states it",
            weight: Some("1000"),
            max_size: 1000,
            units: 1000,
            asked_by: CapacityFrom::Create,
            expected_instances: 1,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        // One emulator per row: each row is its own account, so one row's instances never
        // explain another's.
        let (router, _engine) = emulator();
        let name = format!("asg-weighted-{index}");
        create_weighted_group(&router, &name, row).await;

        let members = settled_members(&router, &name, row.expected_instances, row.case).await;
        assert_eq!(
            members.len(),
            row.expected_instances,
            "case {:?}: expected {} instances for {} units, got {members:?}",
            row.case,
            row.expected_instances,
            row.units
        );
        let (desired_capacity, _) = described_group(&router, &name).await;
        assert_eq!(
            desired_capacity,
            row.units.to_string(),
            "case {:?}: the group holds the capacity asked for, in units, got \
             {desired_capacity:?}",
            row.case
        );
        let expected_weight = row.weight.map(::std::string::ToString::to_string);
        for member in &members {
            assert_eq!(
                member.instance_type, WEIGHTED_INSTANCE_TYPE,
                "case {:?}: the override's instance type, got {member:?}",
                row.case
            );
            assert_eq!(
                member.weighted_capacity, expected_weight,
                "case {:?}: expected the member to report WeightedCapacity \
                 {expected_weight:?}, got {member:?}",
                row.case
            );
        }

        // The other read path reports the same instances, the group they belong to and the
        // same weight: a client reconciling units against members has both sides.
        let ids: Vec<String> = members
            .iter()
            .map(|member| member.instance_id.clone())
            .collect();
        let detailed = described_instances(&router, &ids).await;
        let expected: Vec<(String, String, Option<String>)> = ids
            .iter()
            .map(|instance_id| {
                (
                    instance_id.clone(),
                    name.clone(),
                    expected_weight.clone(),
                )
            })
            .collect();
        assert_eq!(
            detailed, expected,
            "case {:?}: expected DescribeAutoScalingInstances to report {expected:?}, got \
             {detailed:?}",
            row.case
        );
    }
}

/// One row of the weighted scale-in table: the capacity the group is driven down to, and
/// the instances that still fulfil it afterwards.
struct ScaleInRow {
    case: &'static str,
    /// The capacity, in units, the group is scaled in to.
    units: i32,
    expected_instances: usize,
}

/// A scale-in of a weighted group is a scale-in of CAPACITY UNITS: it takes instances out
/// only while what is left still fulfils the capacity asked for, so a group of members
/// worth several units each loses fewer instances than the surplus names.
#[tokio::test]
async fn a_weighted_scale_in_keeps_the_capacity_fulfilled() {
    let rows = [
        ScaleInRow {
            case: "down to three units: two instances of two units still fulfil it",
            units: 3,
            expected_instances: 2,
        },
        ScaleInRow {
            case: "down to two units: one instance of two units fulfils it exactly",
            units: 2,
            expected_instances: 1,
        },
        ScaleInRow {
            case: "down to nothing: the group holds no instance",
            units: 0,
            expected_instances: 0,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let (router, _engine) = emulator();
        let name = format!("asg-weighted-in-{index}");
        // Six units at a weight of two: three instances, which is what each row scales in
        // from.
        let grown = WeightRow {
            case: row.case,
            weight: Some("2"),
            max_size: 8,
            units: 6,
            asked_by: CapacityFrom::SetDesiredCapacity,
            expected_instances: 3,
        };
        create_weighted_group(&router, &name, &grown).await;
        let launched = settled_members(&router, &name, 3, row.case).await;

        let params = vec![
            p("AutoScalingGroupName", name.as_str()),
            p("DesiredCapacity", row.units.to_string()),
        ];
        let (status, body) = query(&router, "SetDesiredCapacity", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {:?}: SetDesiredCapacity to {} units got {status}: {body}",
            row.case, row.units
        );

        let left = settled_members(&router, &name, row.expected_instances, row.case).await;
        assert_eq!(
            left.len(),
            row.expected_instances,
            "case {:?}: expected {} instances left for {} units, got {left:?} of \
             {launched:?}",
            row.case,
            row.expected_instances,
            row.units
        );
        // What stays is a subset of what the group launched, still carrying its weight.
        for member in &left {
            assert!(
                launched
                    .iter()
                    .any(|launched| launched.instance_id == member.instance_id),
                "case {:?}: every instance left is one the group launched {launched:?}, \
                 got {member:?}",
                row.case
            );
            assert_eq!(
                member.weighted_capacity.as_deref(),
                Some("2"),
                "case {:?}: a member of the weighted group reports its weight, got \
                 {member:?}",
                row.case
            );
        }
        let (desired_capacity, _) = described_group(&router, &name).await;
        assert_eq!(
            desired_capacity,
            row.units.to_string(),
            "case {:?}: the group holds the capacity the scale-in asked for, got \
             {desired_capacity:?}",
            row.case
        );
    }
}

/// One row of the account-wide read's own table: the request it makes, and what the read
/// must answer.
struct ReadRow {
    case: &'static str,
    /// How many of the group's instances the request names, or `None` to name none at all.
    named_ids: Option<usize>,
    /// `MaxRecords`, when the request carries one.
    max_records: Option<&'static str>,
    /// How many entries the answer carries.
    expected_entries: usize,
    /// Whether the answer carries a `NextToken` for a further page.
    expects_next_token: bool,
}

/// `DescribeAutoScalingInstances` reports the account's Auto Scaling instances, filtered by
/// the ids the request names and paged by `MaxRecords` — and refuses a request naming more
/// ids than one call may carry, rather than scanning the account's inventory once per id.
#[tokio::test]
async fn the_account_wide_instance_read_is_filtered_and_paged() {
    let (router, _engine) = emulator();
    let name = "asg-instance-read";
    // Three instances at a weight of two, so the read has several members to page over.
    let group = WeightRow {
        case: "three members of a weighted group",
        weight: Some("2"),
        max_size: 6,
        units: 6,
        asked_by: CapacityFrom::Create,
        expected_instances: 3,
    };
    create_weighted_group(&router, name, &group).await;
    let members = settled_members(&router, name, 3, group.case).await;
    let ids: Vec<String> = members
        .iter()
        .map(|member| member.instance_id.clone())
        .collect();

    let rows = [
        ReadRow {
            case: "no ids and no page size: every instance of the account",
            named_ids: None,
            max_records: None,
            expected_entries: 3,
            expects_next_token: false,
        },
        ReadRow {
            case: "one id: only that instance",
            named_ids: Some(1),
            max_records: None,
            expected_entries: 1,
            expects_next_token: false,
        },
        ReadRow {
            case: "a page size below the count: one entry and a token for the rest",
            named_ids: None,
            max_records: Some("1"),
            expected_entries: 1,
            expects_next_token: true,
        },
        ReadRow {
            case: "a page size at the count: every entry and no token",
            named_ids: None,
            max_records: Some("3"),
            expected_entries: 3,
            expects_next_token: false,
        },
    ];

    for row in &rows {
        let mut params: Vec<(String, String)> = Vec::new();
        for (index, instance_id) in ids.iter().take(row.named_ids.unwrap_or(0)).enumerate() {
            params.push(p(
                &format!("InstanceIds.member.{}", index + 1),
                instance_id.as_str(),
            ));
        }
        if let Some(max_records) = row.max_records {
            params.push(p("MaxRecords", max_records));
        }
        let (status, body) =
            query(&router, "DescribeAutoScalingInstances", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected 200, got {status}: {body}",
            row.case
        );
        let reported = tag_value(&body, "AutoScalingInstances");
        let entries = reported
            .split("</member>")
            .filter(|member| member.contains("<InstanceId"))
            .count();
        assert_eq!(
            entries, row.expected_entries,
            "case {:?}: expected {} entries, got {body}",
            row.case, row.expected_entries
        );
        assert_eq!(
            !tag_value(&body, "NextToken").is_empty(),
            row.expects_next_token,
            "case {:?}: expected a NextToken: {}, got {body}",
            row.case,
            row.expects_next_token
        );
        if let Some(named) = row.named_ids {
            for instance_id in ids.iter().take(named) {
                assert!(
                    reported.contains(instance_id.as_str()),
                    "case {:?}: expected the answer to report {instance_id}, got {body}",
                    row.case
                );
            }
        }
    }

    // A page the read never issued a token for is refused, and so is an id list longer
    // than one call may carry ("Array Members: Maximum number of 50 items").
    let params = vec![p("NextToken", "not-a-token-this-service-issued")];
    let (status, body) =
        query(&router, "DescribeAutoScalingInstances", &borrowed(&params)).await;
    assert_eq!(status, 400, "an unusable NextToken: got {status}: {body}");
    assert!(
        body.contains("<Code>InvalidNextToken</Code>"),
        "an unusable NextToken is the modelled InvalidNextToken, got {body}"
    );

    let over_the_limit = autoscaling::state::MAX_INSTANCE_IDS_PER_CALL + 1;
    let params: Vec<(String, String)> = (0..over_the_limit)
        .map(|index| {
            p(
                &format!("InstanceIds.member.{}", index + 1),
                format!("i-{index:017x}"),
            )
        })
        .collect();
    let (status, body) =
        query(&router, "DescribeAutoScalingInstances", &borrowed(&params)).await;
    assert_eq!(
        status, 400,
        "{over_the_limit} ids in one call: got {status}: {body}"
    );
    assert!(
        body.contains("<Code>ValidationError</Code>"),
        "too many ids is a ValidationError, got {body}"
    );
    assert!(
        xml_unescape(&tag_value(&body, "Message")).contains(&format!(
            "at 'instanceIds' failed to satisfy constraint: Member must have length less \
             than or equal to {}",
            autoscaling::state::MAX_INSTANCE_IDS_PER_CALL
        )),
        "the refusal names the member and the bound it missed, got {body}"
    );
}
