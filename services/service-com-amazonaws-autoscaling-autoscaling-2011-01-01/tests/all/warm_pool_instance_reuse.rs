// SPDX-License-Identifier: CC0-1.0

//! The instances an Auto Scaling group's warm pool holds, where a client READS them, and
//! what a scale-in does with an instance when the pool asks to have it back.
//!
//! RULES under test:
//!
//! 1. **The account's own instance read reports a group's warm-pool instances beside its own
//!    members, each under the pool's `Warmed:` lifecycle state — the model's
//!    `AutoScalingInstanceDetails$LifecycleState` publishes those states in the same value
//!    set as `InService`
//!    (`smithy_query(query="doc",shape="AutoScalingInstanceDetails",member="LifecycleState")`)
//!    — while the group's own reader reports its members alone, because "to retrieve
//!    information about the instances in a warm pool, you must call the DescribeWarmPool
//!    API".**
//! 2. **`InstanceReusePolicy$ReuseOnScaleIn` "specifies whether instances in the Auto
//!    Scaling group can be returned to the warm pool on scale in", so a scale-in of a group
//!    whose pool carries it hands the instance it takes out of service BACK to that pool —
//!    under its own id, as far as the pool has room for it — instead of terminating it; a
//!    pool that does not carry it keeps nothing of the scale-in.**
//!
//! Why it matters: a client waits for a warm instance before it scales out, and reads the
//! account's instances to find it. A pool whose instances no instance read reports makes
//! warm-pool pre-provisioning unobservable, and a scale-in that terminates an instance the
//! pool asked to keep throws away the pre-initialization the pool exists for — the next
//! scale-out then waits for a cold launch.
//!
//! The rows differ along what the rules generalise over: the `PoolState` the pool is in (so
//! the `Warmed:` state the instance is reported in differs), whether the reuse policy is
//! absent, off or on, and whether the pool has room for everything the scale-in took.
//!
//! Every wait looks again until the condition holds rather than for a span.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_ec2_amazonec2::state as ec2;

use crate::support::{borrowed, element_body, emulator, p, query, tag_value, tag_values, REGION};

/// The launch source every group here names: one of the templates the harness seeds.
const LAUNCH_TEMPLATE: &str = "lt-1";

/// The `LifecycleState` of an instance that is in service, as the model's own
/// `LifecycleState` enum spells it.
const IN_SERVICE: &str = "InService";

/// The prefix every warm-pool `LifecycleState` the model publishes carries
/// (`Warmed:Pending`, `Warmed:Stopped`, `Warmed:Running`, `Warmed:Hibernated`), written out
/// here rather than taken from the service's own constant.
const WARMED: &str = "Warmed:";

/// How long a wait may take before the test gives up. Generous on purpose: the condition is
/// what is waited for.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long to wait between two looks.
const LOOK_AGAIN: Duration = Duration::from_millis(20);

/// The zone every group of this suite places its instances in, read out of Amazon EC2's own
/// catalog for the Region the harness signs for.
fn zone() -> String {
    ec2::availability_zones_in_region(REGION)
        .first()
        .map(|zone| zone.zone_name.clone())
        .expect("the Region has an Availability Zone")
}

/// One instance the account reports: its id, the group it belongs to and the lifecycle state
/// it is in.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reported {
    id: String,
    lifecycle_state: String,
}

/// Every instance the account's own instance read reports for `group`, filtered by the ids
/// the call named when a case names any.
async fn account_instances(
    case: &str,
    router: &MultiServiceRouter,
    group: &str,
    ids: &[&str],
) -> Vec<Reported> {
    let params: Vec<(String, String)> = ids
        .iter()
        .enumerate()
        .map(|(index, id)| p(&format!("InstanceIds.member.{}", index + 1), *id))
        .collect();
    let (status, body) = query(
        router,
        "DescribeAutoScalingInstances",
        &borrowed(&params),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeAutoScalingInstances expected 200, got {status} with {body}"
    );
    list_of(&body, "AutoScalingInstances")
        .iter()
        .filter(|block| tag_value(block, "AutoScalingGroupName") == group)
        .map(|block| Reported {
            id: tag_value(block, "InstanceId"),
            lifecycle_state: tag_value(block, "LifecycleState"),
        })
        .collect()
}

/// Each `<member>` of the list element `list` in one answer, as its own block — scoped to
/// that element, so a member of one list is never read as a member of another.
fn list_of(xml: &str, list: &str) -> Vec<String> {
    element_body(xml, list)
        .map(|body| tag_values(body, "member"))
        .unwrap_or_default()
}

/// The ids the GROUP's own reader reports as its members, which is a different list: a
/// pooled instance is not one of them.
async fn group_members(case: &str, router: &MultiServiceRouter, group: &str) -> Vec<String> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeAutoScalingGroups expected 200, got {status} with {body}"
    );
    list_of(&body, "Instances")
        .iter()
        .map(|block| tag_value(block, "InstanceId"))
        .collect()
}

/// The instances `group`'s warm pool reports, with the state each is in.
async fn pooled(case: &str, router: &MultiServiceRouter, group: &str) -> Vec<Reported> {
    let (status, body) = query(
        router,
        "DescribeWarmPool",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeWarmPool expected 200, got {status} with {body}"
    );
    list_of(&body, "Instances")
        .iter()
        .map(|block| Reported {
            id: tag_value(block, "InstanceId"),
            lifecycle_state: tag_value(block, "LifecycleState"),
        })
        .collect()
}

/// Look again until `group`'s warm pool reports `wanted` instances, every one of them
/// settled in `state`, or a generous deadline passes.
async fn pool_settled(
    case: &str,
    router: &MultiServiceRouter,
    group: &str,
    wanted: usize,
    state: &str,
) -> Vec<Reported> {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let held = pooled(case, router, group).await;
        let settled = held.len() == wanted
            && held
                .iter()
                .all(|instance| instance.lifecycle_state == state);
        if settled || Instant::now() >= deadline {
            return held;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Look again until the account's instance read reports `wanted` instances for `group`, each
/// settled in a state the pool or the group holds rather than a transient one.
async fn account_settled(
    case: &str,
    router: &MultiServiceRouter,
    group: &str,
    wanted: usize,
    states: &[&str],
) -> Vec<Reported> {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let held = account_instances(case, router, group, &[]).await;
        let settled = held.len() == wanted
            && held
                .iter()
                .all(|instance| states.contains(&instance.lifecycle_state.as_str()));
        if settled || Instant::now() >= deadline {
            return held;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Create one group placed by Availability Zone alone, with the bounds a case needs.
async fn create_group(
    router: &MultiServiceRouter,
    group: &str,
    max_size: i32,
    desired: i32,
) {
    let params = vec![
        p("AutoScalingGroupName", group),
        p("LaunchTemplate.LaunchTemplateName", LAUNCH_TEMPLATE),
        p("MinSize", "0"),
        p("MaxSize", max_size.to_string()),
        p("DesiredCapacity", desired.to_string()),
        p("AvailabilityZones.member.1", zone()),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "creating {group:?}: expected 200, got {status} with {body}"
    );
}

/// Configure `group`'s warm pool: its state, its own minimum, and the reuse policy and
/// prepared capacity a case names.
async fn put_warm_pool(
    case: &str,
    router: &MultiServiceRouter,
    group: &str,
    pool_state: &str,
    min_size: i32,
    reuse_on_scale_in: Option<bool>,
    prepared_capacity: Option<i32>,
) {
    let mut params = vec![
        p("AutoScalingGroupName", group),
        p("MinSize", min_size.to_string()),
        p("PoolState", pool_state),
    ];
    if let Some(reuse) = reuse_on_scale_in {
        params.push(p(
            "InstanceReusePolicy.ReuseOnScaleIn",
            if reuse { "true" } else { "false" },
        ));
    }
    if let Some(prepared) = prepared_capacity {
        params.push(p("MaxGroupPreparedCapacity", prepared.to_string()));
    }
    let (status, body) = query(router, "PutWarmPool", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutWarmPool expected 200, got {status} with {body}"
    );
}

/// Ask `group` for `desired` capacity.
async fn set_desired_capacity(case: &str, router: &MultiServiceRouter, group: &str, desired: i32) {
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

/// The state in the account's Amazon EC2 store of the instance `instance_id` names, read
/// straight out of that store — the account's own view of the same instance.
fn ec2_state(engine: &std::sync::Arc<engine::InMemoryEngine>, instance_id: &str) -> String {
    let state = ec2::service_state(&**engine).expect("the EC2 store is mounted on this engine");
    let store = state.lock();
    store
        .instances
        .get(instance_id)
        .map(|record| record.state.as_str().to_string())
        .unwrap_or_default()
}

/// RULE 1: the account's instance read reports a group's pooled instances beside its own
/// members, each under the `Warmed:` state the pool's own `PoolState` puts it in, while the
/// group's own reader reports its members alone.
#[tokio::test]
async fn the_account_instance_read_reports_the_warm_pool_instances() {
    let (router, _engine) = emulator();
    // The rows differ along the pool's own state, which is what the warmed state an instance
    // settles in is spelled from. Every value the model's `WarmPoolState` enum publishes
    // (`smithy_query(query="shape",shape="WarmPoolState")`) is a row.
    let rows = [
        ("a pool of stopped instances", "asg-wp-read-stopped", "Stopped"),
        ("a pool of running instances", "asg-wp-read-running", "Running"),
        (
            "a pool of hibernated instances",
            "asg-wp-read-hibernated",
            "Hibernated",
        ),
    ];

    for (case, group, pool_state) in rows {
        let warmed = format!("{WARMED}{pool_state}");
        create_group(&router, group, 2, 1).await;
        put_warm_pool(case, &router, group, pool_state, 1, None, None).await;

        // One member of the group's own, and one of its pool's.
        let reported = account_settled(case, &router, group, 2, &[IN_SERVICE, &warmed]).await;
        assert_eq!(
            reported
                .iter()
                .filter(|instance| instance.lifecycle_state == warmed)
                .count(),
            1,
            "case {case:?}: expected exactly one instance reported in {warmed:?}, got \
             {reported:?}"
        );
        assert_eq!(
            reported
                .iter()
                .filter(|instance| instance.lifecycle_state == IN_SERVICE)
                .count(),
            1,
            "case {case:?}: expected exactly one instance reported in {IN_SERVICE:?}, got \
             {reported:?}"
        );

        // It is the SAME instance the pool's own reader reports, and asking the account read
        // for that one id answers it.
        let pool = pool_settled(case, &router, group, 1, &warmed).await;
        let pooled_id = pool
            .first()
            .map(|instance| instance.id.clone())
            .unwrap_or_default();
        assert!(
            reported
                .iter()
                .any(|instance| instance.id == pooled_id && instance.lifecycle_state == warmed),
            "case {case:?}: expected the pooled instance {pooled_id:?} among the account's \
             instances, got {reported:?}"
        );
        assert_eq!(
            account_instances(case, &router, group, &[pooled_id.as_str()]).await,
            vec![Reported {
                id: pooled_id.clone(),
                lifecycle_state: warmed.clone(),
            }],
            "case {case:?}: expected the account read to answer the pooled instance alone when \
             the request names its id"
        );

        // ...and the GROUP's own reader reports its members alone: the pooled instance is
        // read through the pool.
        let members = group_members(case, &router, group).await;
        assert!(
            !members.contains(&pooled_id) && members.len() == 1,
            "case {case:?}: expected the group to report its one member and not the pooled \
             instance {pooled_id:?}, got {members:?}"
        );
    }
}

/// One row of the reuse table: what the pool's reuse policy says, and whether the instance a
/// scale-in takes out of service is expected back in the pool.
struct Row {
    case: &'static str,
    group: &'static str,
    /// `None` leaves `InstanceReusePolicy` out of the put altogether.
    reuse_on_scale_in: Option<bool>,
    returns_to_the_pool: bool,
}

/// RULE 2: a scale-in hands the instance it takes out of service back to a pool whose reuse
/// policy asks for it, under that instance's own id; a pool that does not ask for it keeps
/// nothing of the scale-in, and the instance is terminated where it lives.
#[tokio::test]
async fn a_scale_in_returns_its_instance_to_a_pool_that_reuses_it() {
    let (router, engine) = emulator();
    let warmed = format!("{WARMED}Stopped");
    let rows = [
        Row {
            case: "a pool that reuses the instances a scale-in takes",
            group: "asg-wp-reuse-on",
            reuse_on_scale_in: Some(true),
            returns_to_the_pool: true,
        },
        Row {
            case: "a pool whose reuse policy says not to",
            group: "asg-wp-reuse-off",
            reuse_on_scale_in: Some(false),
            returns_to_the_pool: false,
        },
        Row {
            case: "a pool carrying no reuse policy at all",
            group: "asg-wp-reuse-absent",
            reuse_on_scale_in: None,
            returns_to_the_pool: false,
        },
    ];

    for row in rows {
        let case = row.case;
        // A group holding one instance of its own, beside a pool holding one: the pool
        // maintains the difference between the group's maximum and its desired capacity.
        create_group(&router, row.group, 2, 1).await;
        put_warm_pool(case, &router, row.group, "Stopped", 1, row.reuse_on_scale_in, None).await;
        let prepared = pool_settled(case, &router, row.group, 1, &warmed).await;
        let prepared_id = prepared
            .first()
            .map(|instance| instance.id.clone())
            .unwrap_or_default();
        assert!(
            !prepared_id.is_empty(),
            "case {case:?}: expected the pool to prepare one instance, got {prepared:?}"
        );
        // The group's own member is IN SERVICE before the scale-in: the pool takes over an
        // instance that is in service, so a case must not race the launch that put it there.
        let settled = account_settled(case, &router, row.group, 2, &[IN_SERVICE, &warmed]).await;
        assert_eq!(
            settled
                .iter()
                .filter(|instance| instance.lifecycle_state == IN_SERVICE)
                .count(),
            1,
            "case {case:?}: expected the group's own member to be in service, got {settled:?}"
        );
        let members = group_members(case, &router, row.group).await;
        assert_eq!(
            members.len(),
            1,
            "case {case:?}: expected the group to hold one member of its own, got {members:?}"
        );
        let scaled_in = members[0].clone();

        // The scale-in: the group gives up its only member, and the pool now maintains two.
        set_desired_capacity(case, &router, row.group, 0).await;
        let pool = pool_settled(case, &router, row.group, 2, &warmed).await;
        let held: BTreeSet<&str> = pool.iter().map(|instance| instance.id.as_str()).collect();
        assert!(
            held.contains(prepared_id.as_str()),
            "case {case:?}: expected the pool to keep the instance it had prepared, got {pool:?}"
        );
        assert_eq!(
            held.contains(scaled_in.as_str()),
            row.returns_to_the_pool,
            "case {case:?}: expected the instance the scale-in took ({scaled_in:?}) {} the \
             pool, got {pool:?}",
            if row.returns_to_the_pool {
                "to be in"
            } else {
                "to be kept out of"
            }
        );
        // The group itself holds none of them either way: the instance left service.
        let members = group_members(case, &router, row.group).await;
        assert!(
            members.is_empty(),
            "case {case:?}: expected the group to hold no member of its own, got {members:?}"
        );
        // And the account's own Amazon EC2 view agrees about the instance: a returned one is
        // held stopped like the rest of the pool, while one the pool did not take is shut
        // down.
        let ec2_state = ec2_state(&engine, &scaled_in);
        let expected: &[&str] = if row.returns_to_the_pool {
            &["stopping", "stopped"]
        } else {
            &["shutting-down", "terminated", ""]
        };
        assert!(
            expected.contains(&ec2_state.as_str()),
            "case {case:?}: expected the instance the scale-in took to be in one of \
             {expected:?} in Amazon EC2, got {ec2_state:?}"
        );
    }
}

/// RULE 2, at its bound: a pool takes back only what it has ROOM for, so a scale-in of more
/// instances than the pool maintains leaves the rest to be terminated.
#[tokio::test]
async fn a_pool_takes_back_only_what_it_maintains() {
    let (router, _engine) = emulator();
    let case = "a scale-in of two into a pool that maintains one";
    let group = "asg-wp-reuse-bounded";
    let warmed = format!("{WARMED}Stopped");

    // The pool's size is "the difference between the MaxGroupPreparedCapacity and the desired
    // capacity" (`PutWarmPoolType$MaxGroupPreparedCapacity`), so at a desired capacity of 3
    // and a prepared capacity of 2 the pool maintains nothing at all...
    create_group(&router, group, 4, 3).await;
    put_warm_pool(case, &router, group, "Stopped", 0, Some(true), Some(2)).await;
    // The three launches are IN SERVICE before the scale-in, so what the pool takes back is
    // not decided by a race with them.
    let settled = account_settled(case, &router, group, 3, &[IN_SERVICE]).await;
    assert_eq!(
        settled.len(),
        3,
        "case {case:?}: expected the group's three members to be in service, got {settled:?}"
    );
    let members = group_members(case, &router, group).await;
    assert_eq!(
        members.len(),
        3,
        "case {case:?}: expected the group to hold three members of its own, got {members:?}"
    );
    let before: BTreeSet<String> = members.iter().cloned().collect();

    // ...and at a desired capacity of 1 it maintains exactly one, while the scale-in takes
    // two instances out of service: one goes back to the pool and the other is terminated.
    set_desired_capacity(case, &router, group, 1).await;
    let pool = pool_settled(case, &router, group, 1, &warmed).await;
    assert_eq!(
        pool.len(),
        1,
        "case {case:?}: expected the pool to hold the one instance it maintains, got {pool:?}"
    );
    let left = group_members(case, &router, group).await;
    assert_eq!(
        left.len(),
        1,
        "case {case:?}: expected the group to hold its one remaining member, got {left:?}"
    );
    let kept: BTreeSet<String> = left
        .iter()
        .cloned()
        .chain(pool.iter().map(|instance| instance.id.clone()))
        .collect();
    assert!(
        kept.len() == 2 && kept.iter().all(|id| before.contains(id)),
        "case {case:?}: expected the member left in service and the one returned to the pool \
         to both be instances the group already held ({before:?}), got {kept:?}"
    );
}
