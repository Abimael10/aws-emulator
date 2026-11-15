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

use std::sync::Arc;
use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;
use service_com_amazonaws_ec2_amazonec2 as ec2_service;
use service_com_amazonaws_ec2_amazonec2::state as ec2;

use crate::support::{borrowed, ec2_query, p, query, tag_value, tag_values, REGION};

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
