// SPDX-License-Identifier: CC0-1.0

//! Which Availability Zone a SCALE-IN of an Auto Scaling group takes its instance from.
//!
//! RULE under test: **a capacity decrease chooses each instance it takes out of service
//! from the Availability Zone(s) holding the most of the group's instances — of the zones
//! that still hold an instance the scale-in may take — and the group's own
//! `TerminationPolicies` ("a policy or a list of policies that are used to select the
//! instance to terminate. These policies are executed in the order that you list them",
//! `smithy_query(query="doc",shape="CreateAutoScalingGroupType",member="TerminationPolicies")`)
//! order the candidates only WITHIN that zone. The spread is re-read between one instance
//! and the next, so a decrease of several takes each from whichever zone is fullest at that
//! point.**
//!
//! Why it matters: a group is configured for several zones to survive the loss of one. A
//! scale-in that applied the termination policy across the whole group would empty the
//! thinner zone first — the instance standing alone in it is exactly the one the group was
//! spread for — and repeated decreases would leave every instance in one failure domain.
//!
//! The rows differ along what the rule generalises over: which termination policy the group
//! names (and naming none at all), how the group is spread over its two zones and which
//! zone is the fuller one, how far the group is scaled in, and whether the policy's own
//! preference agrees with the zone step or disagrees with it. A single-zone row is a row
//! too: with one zone there is nothing to balance and the policy decides alone.
//!
//! Every wait here looks again until the group has settled rather than for a span: the
//! lifecycle transitions are owed on a clock, so a fixed sleep would be a guess about how
//! fast the host runs.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_ec2_amazonec2::state as ec2;

use crate::support::{borrowed, element_body, emulator, p, query, tag_value, tag_values, REGION};

/// The launch source every group here names: one of the templates the harness seeds, since
/// what these cases are about is WHICH instance a scale-in picks rather than what it runs.
const LAUNCH_TEMPLATE: &str = "lt-1";

/// The `LifecycleState` of an instance that is in service, spelled as the model's own
/// `com.amazonaws.autoscaling#LifecycleState` enum spells it
/// (`smithy_query(query="shape",shape="LifecycleState")`).
const IN_SERVICE: &str = "InService";

/// The largest capacity any group of this suite is configured for, which leaves room for
/// every row's launches.
const MAX_SIZE: i32 = 6;

/// How long a wait for the group to settle may take before the test gives up. Generous on
/// purpose: the condition is what is waited for, and a loaded host may need many passes.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long to wait between two looks while waiting for a condition.
const LOOK_AGAIN: Duration = Duration::from_millis(20);

/// The two Availability Zones of the Region the harness signs for that this suite spreads
/// its groups over, read out of Amazon EC2's own catalog for that Region rather than
/// written out here.
fn zone_pair() -> [String; 2] {
    let zones = ec2::availability_zones_in_region(REGION);
    let mut names = zones.iter().map(|zone| zone.zone_name.clone());
    [
        names.next().expect("the Region has an Availability Zone"),
        names.next().expect("the Region has a second Availability Zone"),
    ]
}

/// One instance of a group as this suite reads it back: its id and the zone it stands in.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Member {
    id: String,
    zone: String,
    lifecycle_state: String,
}

/// Every instance the account reports for `group`, in the order the service reports them.
async fn members(case: &str, router: &MultiServiceRouter, group: &str) -> Vec<Member> {
    let (status, body) = query(router, "DescribeAutoScalingInstances", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeAutoScalingInstances expected 200, got {status} with {body}"
    );
    element_body(&body, "AutoScalingInstances")
        .map(|list| tag_values(list, "member"))
        .unwrap_or_default()
        .iter()
        .filter(|block| tag_value(block, "AutoScalingGroupName") == group)
        .map(|block| Member {
            id: tag_value(block, "InstanceId"),
            zone: tag_value(block, "AvailabilityZone"),
            lifecycle_state: tag_value(block, "LifecycleState"),
        })
        .collect()
}

/// Look again until `group` reports exactly `wanted` instances and every one of them is in
/// service, or a generous deadline passes.
async fn settled(
    case: &str,
    router: &MultiServiceRouter,
    group: &str,
    wanted: usize,
) -> Vec<Member> {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let held = members(case, router, group).await;
        let settled = held.len() == wanted
            && held
                .iter()
                .all(|member| member.lifecycle_state == IN_SERVICE);
        if settled || Instant::now() >= deadline {
            return held;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Create `group` placed in `zone` alone, holding nothing yet, with the termination
/// policies a case names.
async fn create_group(
    router: &MultiServiceRouter,
    group: &str,
    zone: &str,
    policies: &[&str],
) {
    let mut params = vec![
        p("AutoScalingGroupName", group),
        p("LaunchTemplate.LaunchTemplateName", LAUNCH_TEMPLATE),
        p("MinSize", "0"),
        p("MaxSize", MAX_SIZE.to_string()),
        p("DesiredCapacity", "0"),
        p("AvailabilityZones.member.1", zone),
    ];
    for (index, policy) in policies.iter().enumerate() {
        params.push(p(
            &format!("TerminationPolicies.member.{}", index + 1),
            *policy,
        ));
    }
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "creating {group:?}: expected 200, got {status} with {body}"
    );
}

/// Place `group` in `zones` from now on: the zones a group is configured for are where its
/// NEXT launches go, so this is how a case lands its instances in the zone it means.
async fn place_in(case: &str, router: &MultiServiceRouter, group: &str, zones: &[&str]) {
    let mut params = vec![p("AutoScalingGroupName", group)];
    for (index, zone) in zones.iter().enumerate() {
        params.push(p(
            &format!("AvailabilityZones.member.{}", index + 1),
            *zone,
        ));
    }
    let (status, body) = query(router, "UpdateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: placing {group:?} in {zones:?} expected 200, got {status} with {body}"
    );
}

/// Ask `group` for `desired` capacity.
async fn set_desired_capacity(case: &str, router: &MultiServiceRouter, group: &str, desired: usize) {
    let (status, body) = query(
        router,
        "SetDesiredCapacity",
        &[
            ("AutoScalingGroupName", group),
            ("DesiredCapacity", &desired.to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SetDesiredCapacity({desired}) expected 200, got {status} with {body}"
    );
}

/// Build `group`'s membership one wave at a time and answer the instances in LAUNCH order:
/// each wave places the group in one zone and raises its capacity by one, so the instance
/// it launches lands in that zone and is strictly newer than the ones before it.
async fn launch_waves(
    case: &str,
    router: &MultiServiceRouter,
    group: &str,
    zones: &[&str],
    waves: &[usize],
) -> Vec<Member> {
    let mut launched: Vec<Member> = Vec::new();
    for (step, wave) in waves.iter().enumerate() {
        let zone = zones[*wave];
        place_in(case, router, group, &[zone]).await;
        set_desired_capacity(case, router, group, step + 1).await;
        let held = settled(case, router, group, step + 1).await;
        assert_eq!(
            held.len(),
            step + 1,
            "case {case:?}: expected {} instance(s) in service after wave {step}, got {held:?}",
            step + 1
        );
        let known: BTreeSet<&str> = launched.iter().map(|member| member.id.as_str()).collect();
        let fresh: Vec<Member> = held
            .iter()
            .filter(|member| !known.contains(member.id.as_str()))
            .cloned()
            .collect();
        assert_eq!(
            fresh.len(),
            1,
            "case {case:?}: wave {step} must have launched exactly one instance, got {fresh:?}"
        );
        assert_eq!(
            fresh[0].zone, zone,
            "case {case:?}: wave {step} expected its instance in {zone}, got {:?}",
            fresh[0]
        );
        launched.extend(fresh);
    }
    launched
}

/// One row of the zonal-balance table.
struct Row {
    case: &'static str,
    group: &'static str,
    /// Which zone each successive launch goes into, by index into the zone pair.
    waves: &'static [usize],
    policies: &'static [&'static str],
    /// The capacity the group is scaled in to.
    scale_in_to: usize,
    /// Which of the launched instances survive, by their index in launch order.
    survivors: &'static [usize],
}

/// RULE: the zone holding the most of the group's instances is where each instance a
/// scale-in takes comes from, and the group's `TerminationPolicies` order the candidates
/// inside it.
#[tokio::test]
async fn a_scale_in_takes_its_instance_from_the_fullest_availability_zone() {
    let (router, _engine) = emulator();
    let [first, second] = zone_pair();
    let zones = [first.as_str(), second.as_str()];

    let rows = [
        Row {
            case: "OldestInstance over a 1/2 spread: the fuller zone's older instance leaves \
                   and the globally oldest one, alone in the thinner zone, survives",
            group: "asg-zonal-oldest-thin-first",
            waves: &[0, 1, 1],
            policies: &["OldestInstance"],
            scale_in_to: 2,
            survivors: &[0, 2],
        },
        Row {
            case: "the same spread the other way round: the fuller zone is the one the group \
                   was placed in first",
            group: "asg-zonal-oldest-fat-first",
            waves: &[0, 0, 1],
            policies: &["OldestInstance"],
            scale_in_to: 2,
            survivors: &[1, 2],
        },
        Row {
            case: "NewestInstance over a 2/1 spread: the globally newest instance stands alone \
                   in the thinner zone, so the fuller zone's newer one leaves",
            group: "asg-zonal-newest",
            waves: &[0, 0, 1],
            policies: &["NewestInstance"],
            scale_in_to: 2,
            survivors: &[0, 2],
        },
        Row {
            case: "no termination policy at all: the fuller zone's most recently launched \
                   instance leaves, and the globally newest one in the thinner zone survives",
            group: "asg-zonal-default",
            waves: &[1, 0, 0],
            policies: &[],
            scale_in_to: 2,
            survivors: &[0, 1],
        },
        Row {
            case: "a scale-in of two over a 1/3 spread takes both from the fuller zone",
            group: "asg-zonal-two-from-the-fat-zone",
            waves: &[0, 1, 1, 1],
            policies: &["OldestInstance"],
            scale_in_to: 2,
            survivors: &[0, 3],
        },
        Row {
            case: "a scale-in of two over an even spread takes one from each zone, because the \
                   spread is re-read between the two",
            group: "asg-zonal-one-from-each",
            waves: &[0, 0, 1, 1],
            policies: &["OldestInstance"],
            scale_in_to: 2,
            survivors: &[1, 3],
        },
        Row {
            case: "one zone: nothing to balance, so OldestInstance decides alone",
            group: "asg-zonal-single-zone",
            waves: &[0, 0, 0],
            policies: &["OldestInstance"],
            scale_in_to: 2,
            survivors: &[1, 2],
        },
        Row {
            case: "a list whose first policy decides inside the chosen zone",
            group: "asg-zonal-policy-list",
            waves: &[0, 1, 1],
            policies: &["OldestInstance", "NewestInstance"],
            scale_in_to: 2,
            survivors: &[0, 2],
        },
    ];

    for row in rows {
        let case = row.case;
        create_group(&router, row.group, zones[row.waves[0]], row.policies).await;
        let launched = launch_waves(case, &router, row.group, &zones, row.waves).await;
        // The group is placed in BOTH zones before the decrease, so nothing but the spread
        // it already holds decides where the scale-in takes its instance from.
        place_in(case, &router, row.group, &zones).await;

        set_desired_capacity(case, &router, row.group, row.scale_in_to).await;
        let left = settled(case, &router, row.group, row.scale_in_to).await;
        let survivors: BTreeSet<&str> = left.iter().map(|member| member.id.as_str()).collect();
        let expected: BTreeSet<&str> = row
            .survivors
            .iter()
            .map(|index| launched[*index].id.as_str())
            .collect();
        assert_eq!(
            survivors, expected,
            "case {case:?}: expected the instances launched at {:?} to survive, got {left:?} \
             out of {launched:?}",
            row.survivors
        );
    }
}

/// RULE: a zone counts as a candidate zone only while it holds an instance the scale-in MAY
/// take, so a fuller zone whose instances are all protected from scale-in is passed over and
/// the policy decides among the instances that are left.
#[tokio::test]
async fn a_zone_holding_only_protected_instances_is_not_the_zone_scaled_in() {
    let (router, _engine) = emulator();
    let [first, second] = zone_pair();
    let zones = [first.as_str(), second.as_str()];
    let case = "the fuller zone's instances are both protected from scale-in";
    let group = "asg-zonal-protected";

    create_group(&router, group, zones[0], &["OldestInstance"]).await;
    // One instance in the thinner zone, launched first and so the globally oldest, and two
    // in the zone that ends up fuller.
    let launched = launch_waves(case, &router, group, &zones, &[0, 1, 1]).await;
    place_in(case, &router, group, &zones).await;

    let protected: Vec<&str> = launched[1..]
        .iter()
        .map(|member| member.id.as_str())
        .collect();
    let mut params = vec![
        p("AutoScalingGroupName", group),
        p("ProtectedFromScaleIn", "true"),
    ];
    for (index, instance_id) in protected.iter().enumerate() {
        params.push(p(&format!("InstanceIds.member.{}", index + 1), *instance_id));
    }
    let (status, body) = query(&router, "SetInstanceProtection", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: SetInstanceProtection expected 200, got {status} with {body}"
    );

    set_desired_capacity(case, &router, group, 2).await;
    let left = settled(case, &router, group, 2).await;
    let survivors: BTreeSet<&str> = left.iter().map(|member| member.id.as_str()).collect();
    let expected: BTreeSet<&str> = protected.iter().copied().collect();
    assert_eq!(
        survivors, expected,
        "case {case:?}: expected the two protected instances to survive and the thinner \
         zone's unprotected one to leave, got {left:?} out of {launched:?}"
    );
}
