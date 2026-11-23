// SPDX-License-Identifier: CC0-1.0

//! A scale-out of a group that has a populated warm pool CONSUMES the pool.
//!
//! The rule under test: when an Auto Scaling group has to add instances to reach the
//! capacity it maintains, it takes the instances its warm pool has already prepared —
//! oldest prepared first, each keeping its own `InstanceId` — into service through
//! `Pending` before it launches anything new, and the pool's depth falls by the number
//! consumed. The model spells the warmed states (`Warmed:Stopped`, `Warmed:Running`,
//! `Warmed:Hibernated`) in the SAME `LifecycleState` value set as `Pending` and
//! `InService` (`smithy_query(query="shape",shape="LifecycleState")`), i.e. a warmed
//! instance is a member of that group that later transitions into service — which is what
//! "a pool of pre-initialized EC2 instances" is for.
//!
//! The table below spans what that rule generalises over: how deep the pool is, how far the
//! capacity rises (less than the pool's depth, exactly it, and past it so the remainder is a
//! fresh launch), which request raises it (`SetDesiredCapacity` and
//! `UpdateAutoScalingGroup`), every published `PoolState`, and whether the pool refills
//! afterwards or is left empty by the documented size rule. Every row records the prepared
//! ids BEFORE the raise and compares them with the ids the group reports in service after
//! it, and resolves each consumed id in the account's own Amazon EC2 state, so no row can
//! pass on a fresh launch that merely reaches the right count.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;
use service_com_amazonaws_ec2_amazonec2::state as ec2;

use crate::support::{
    borrowed, create_launch_template, ec2_query, emulator, list_members, p, query, seed_subnets,
    tag_value, tag_values,
};

/// How often a poll re-reads the state it is waiting on.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How long a poll keeps trying before it gives up: the lifecycle span this service owes a
/// transition and the one Amazon EC2 owes, with ample margin for a loaded test machine.
/// Derived from the services' own spans rather than written out, so they cannot drift apart.
fn poll_budget() -> Duration {
    autoscaling::state::LIFECYCLE_TRANSITION.max(ec2::TRANSITION_DELAY) + Duration::from_secs(10)
}

/// The `LifecycleState` an instance of a pool in `pool_state` is reported in once the pool's
/// own transition is due: the pool state under the prefix the model's `LifecycleState` enum
/// spells warm-pool states with.
fn warmed(pool_state: &str) -> String {
    format!("Warmed:{pool_state}")
}

/// The `LifecycleState` of an instance that has reached service, from that same enum.
const IN_SERVICE: &str = "InService";

/// The published `WarmPoolState` values, from the model's own enum
/// (`smithy_query(query="shape",shape="WarmPoolState")`): `Stopped | Running | Hibernated`.
const POOL_STATES: [&str; 3] = ["Stopped", "Running", "Hibernated"];

/// One row of the consumption table.
struct Row {
    case: &'static str,
    /// The group's `MaxSize`; it is created with `MinSize` 0 and `DesiredCapacity` 0.
    max_size: i32,
    /// `MinSize` of the pool, when the request carries one.
    pool_min_size: Option<i32>,
    /// `MaxGroupPreparedCapacity`, when the request carries one.
    prepared_capacity: Option<i32>,
    pool_state: &'static str,
    /// How many instances the pool must prepare before the raise, by the documented size
    /// rule (the prepared capacity, or the group's maximum when none is set, less the
    /// group's desired capacity, with the pool's own minimum as the floor).
    pool_depth: usize,
    /// The capacity the raise asks for.
    raise_to: i32,
    /// The request that raises it.
    raise_with: &'static str,
    /// How many of the prepared instances that raise must consume.
    consumed: usize,
    /// How deep the pool is once the raise has settled, by that same size rule.
    depth_after: usize,
}

/// Every row: the raise draws the instances it needs from the pool before launching
/// anything, oldest prepared first, and the pool loses exactly those instances.
#[tokio::test]
async fn a_scale_out_consumes_the_warm_pool_before_launching_anything() {
    let rows = [
        Row {
            case: "a raise the pool covers exactly, from a stopped pool that refills",
            max_size: 2,
            pool_min_size: Some(2),
            prepared_capacity: None,
            pool_state: POOL_STATES[0],
            pool_depth: 2,
            raise_to: 2,
            raise_with: "SetDesiredCapacity",
            consumed: 2,
            // The pool's own minimum is the floor under its size, so it prepares two more.
            depth_after: 2,
        },
        Row {
            case: "a raise past the pool's depth: the rest is a fresh launch",
            max_size: 3,
            pool_min_size: Some(1),
            prepared_capacity: Some(1),
            pool_state: POOL_STATES[0],
            pool_depth: 1,
            raise_to: 2,
            raise_with: "SetDesiredCapacity",
            consumed: 1,
            depth_after: 1,
        },
        Row {
            case: "a raise of one against a running pool of two, through the group's update",
            max_size: 3,
            pool_min_size: None,
            prepared_capacity: Some(2),
            pool_state: POOL_STATES[1],
            pool_depth: 2,
            raise_to: 1,
            raise_with: "UpdateAutoScalingGroup",
            consumed: 1,
            // The prepared capacity less the new desired capacity: the one instance the
            // raise left in the pool, with nothing to launch beside it.
            depth_after: 1,
        },
        Row {
            case: "a hibernated pool consumed whole, leaving it empty",
            max_size: 4,
            pool_min_size: None,
            prepared_capacity: Some(2),
            pool_state: POOL_STATES[2],
            pool_depth: 2,
            raise_to: 2,
            raise_with: "UpdateAutoScalingGroup",
            consumed: 2,
            depth_after: 0,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        // One emulator per row: each row is its own account, so the ids of one row never
        // explain another's.
        let (router, engine) = emulator();
        let subnets = seed_subnets(&engine, 2);
        let template = format!("lt-consume-{index}");
        create_launch_template(&router, &template).await;
        let group = format!("asg-consume-{index}");
        create_group(
            &router,
            &group,
            &template,
            &format!("{},{}", subnets[0].id, subnets[1].id),
            row.max_size,
        )
        .await;

        let mut put = vec![
            p("AutoScalingGroupName", group.clone()),
            p("PoolState", row.pool_state),
        ];
        if let Some(min_size) = row.pool_min_size {
            put.push(p("MinSize", min_size.to_string()));
        }
        if let Some(prepared) = row.prepared_capacity {
            put.push(p("MaxGroupPreparedCapacity", prepared.to_string()));
        }
        let (status, body) = query(&router, "PutWarmPool", &borrowed(&put)).await;
        assert_eq!(
            status, 200,
            "case {:?}: PutWarmPool expected 200, got {status}: {body}",
            row.case
        );

        // The instances the pool has PREPARED, in the order it holds them, read before the
        // raise: these are the ids the raise has to draw from.
        let prepared_state = warmed(row.pool_state);
        let prepared = settled_pool(&router, row.case, &group, row.pool_depth, &prepared_state).await;
        assert_eq!(
            prepared.len(),
            row.pool_depth,
            "case {:?}: expected the pool to prepare {} instance(s) in {prepared_state}, got \
             {prepared:?}",
            row.case,
            row.pool_depth
        );

        let (status, body) = query(
            &router,
            row.raise_with,
            &[
                ("AutoScalingGroupName", group.as_str()),
                ("DesiredCapacity", row.raise_to.to_string().as_str()),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: {} expected 200, got {status}: {body}",
            row.case, row.raise_with
        );

        // The instances the group reports IN SERVICE once the raise has settled.
        let in_service = settled_group(
            &router,
            row.case,
            &group,
            usize::try_from(row.raise_to).expect("a non-negative capacity"),
        )
        .await;
        let drawn: Vec<String> = prepared
            .iter()
            .filter(|id| in_service.contains(id))
            .cloned()
            .collect();
        // Oldest prepared first: the instances drawn are the FRONT of the pool's own order.
        let expected_drawn: Vec<String> = prepared.iter().take(row.consumed).cloned().collect();
        assert_eq!(
            drawn, expected_drawn,
            "case {:?}: expected the raise to draw {expected_drawn:?} out of the pool \
             {prepared:?}, got {drawn:?} in service {in_service:?}",
            row.case
        );
        let fresh: Vec<String> = in_service
            .iter()
            .filter(|id| !prepared.contains(id))
            .cloned()
            .collect();
        assert_eq!(
            fresh.len(),
            usize::try_from(row.raise_to)
                .expect("a non-negative capacity")
                .saturating_sub(row.consumed),
            "case {:?}: expected only the capacity the pool could not cover to be launched \
             fresh, got {fresh:?} beside the pool {prepared:?}",
            row.case
        );

        // The pool LOST exactly those instances — whatever it has prepared since — and is
        // back at the depth the documented size rule gives it at the new capacity.
        let pool_after = settled_pool(
            &router,
            row.case,
            &group,
            row.depth_after,
            &prepared_state,
        )
        .await;
        assert_eq!(
            pool_after.len(),
            row.depth_after,
            "case {:?}: expected a pool of {} after the raise, got {pool_after:?}",
            row.case,
            row.depth_after
        );
        for id in &drawn {
            assert!(
                !pool_after.contains(id),
                "case {:?}: the consumed instance {id} must have left the pool, got \
                 {pool_after:?}",
                row.case
            );
        }

        // And each consumed instance is RUNNING where it lives: an instance in service is
        // one Amazon EC2 reports running, whichever state the pool was keeping it in.
        for id in &drawn {
            let state = ec2_instance_settled(&router, id, "running").await;
            assert_eq!(
                state.as_deref(),
                Some("running"),
                "case {:?}: the consumed instance {id} must be running in Amazon EC2, got \
                 {state:?}",
                row.case
            );
        }
    }
}

/// A raise a group with an EMPTY warm pool makes launches fresh instances: the rule is
/// about the instances the pool has PREPARED, so a pool holding none covers nothing.
#[tokio::test]
async fn a_scale_out_launches_fresh_instances_when_the_pool_has_prepared_none() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    create_launch_template(&router, "lt-consume-empty").await;
    create_group(
        &router,
        "asg-consume-empty",
        "lt-consume-empty",
        &subnets[0].id,
        2,
    )
    .await;

    // A pool whose prepared capacity is below the group's desired capacity maintains none
    // ("if the desired capacity of the Auto Scaling group is higher than the
    // MaxGroupPreparedCapacity, the capacity of the warm pool is 0",
    // `smithy_query(query="doc",shape="PutWarmPoolType",member="MaxGroupPreparedCapacity")`).
    let (status, body) = query(
        &router,
        "PutWarmPool",
        &[
            ("AutoScalingGroupName", "asg-consume-empty"),
            ("MaxGroupPreparedCapacity", "1"),
            ("MinSize", "0"),
        ],
    )
    .await;
    assert_eq!(status, 200, "PutWarmPool expected 200, got {status}: {body}");

    let before = settled_pool(
        &router,
        "an empty pool",
        "asg-consume-empty",
        1,
        &warmed(POOL_STATES[0]),
    )
    .await;
    assert_eq!(
        before.len(),
        1,
        "the pool prepares its one instance before the raise, got {before:?}"
    );

    // Raising past the prepared capacity leaves the pool maintaining none, so the one
    // instance it prepared is consumed and the second unit is a fresh launch.
    let (status, body) = query(
        &router,
        "SetDesiredCapacity",
        &[
            ("AutoScalingGroupName", "asg-consume-empty"),
            ("DesiredCapacity", "2"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "SetDesiredCapacity expected 200, got {status}: {body}"
    );
    let in_service = settled_group(&router, "an empty pool", "asg-consume-empty", 2).await;
    assert!(
        in_service.contains(&before[0]),
        "the prepared instance {:?} must be among the two in service, got {in_service:?}",
        before[0]
    );
    let pool_after = settled_pool(
        &router,
        "an empty pool",
        "asg-consume-empty",
        0,
        &warmed(POOL_STATES[0]),
    )
    .await;
    assert!(
        pool_after.is_empty(),
        "a pool maintaining none holds none once its instance was consumed, got {pool_after:?}"
    );
}

/// One group, created with a launch template of the account, placed in `placement` (one or
/// more subnet ids), with `MinSize` 0 and `DesiredCapacity` 0 — the capacity every row
/// raises FROM.
async fn create_group(
    router: &MultiServiceRouter,
    name: &str,
    template: &str,
    placement: &str,
    max_size: i32,
) {
    let params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", template),
        p("MinSize", "0"),
        p("MaxSize", max_size.to_string()),
        p("DesiredCapacity", "0"),
        p("VPCZoneIdentifier", placement),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "CreateAutoScalingGroup for {name} expected 200, got {status}: {body}"
    );
}

/// Read `group`'s warm pool until it holds `wanted` instances all reporting `state`, or a
/// generous deadline passes — the CONDITION, never a fixed wait. Answers the ids it last
/// reported, in the order the pool holds them.
async fn settled_pool(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    wanted: usize,
    state: &str,
) -> Vec<String> {
    let deadline = Instant::now() + poll_budget();
    loop {
        let (status, body) = query(
            router,
            "DescribeWarmPool",
            &[("AutoScalingGroupName", group)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeWarmPool expected 200, got {status}: {body}"
        );
        let ids = tag_values(&body, "InstanceId");
        let states = tag_values(&body, "LifecycleState");
        let settled =
            ids.len() == wanted && states.len() == wanted && states.iter().all(|held| held == state);
        if settled || Instant::now() >= deadline {
            return ids;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Read `group` back through `DescribeAutoScalingGroups` until it reports `wanted`
/// instances, every one of them `InService`, or a generous deadline passes. Answers the ids
/// it last reported.
async fn settled_group(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    wanted: usize,
) -> Vec<String> {
    let deadline = Instant::now() + poll_budget();
    loop {
        let (status, body) = query(
            router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", group)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeAutoScalingGroups expected 200, got {status}: {body}"
        );
        let members = list_members(&body, "Instances");
        let in_service: Vec<String> = members
            .iter()
            .filter(|member| tag_value(member, "LifecycleState") == IN_SERVICE)
            .map(|member| tag_value(member, "InstanceId"))
            .collect();
        if (in_service.len() == wanted && members.len() == wanted) || Instant::now() >= deadline {
            return in_service;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Wait until Amazon EC2 reports `instance_id` in `state`, or a generous deadline passes;
/// answers what it last reported, and `None` when EC2 resolves the id to no instance.
async fn ec2_instance_settled(
    router: &MultiServiceRouter,
    instance_id: &str,
    state: &str,
) -> Option<String> {
    let deadline = Instant::now() + poll_budget();
    loop {
        let (status, body) =
            ec2_query(router, "DescribeInstances", &[p("InstanceId.1", instance_id)]).await;
        let held = (status == 200)
            .then(|| tag_value(&tag_value(&body, "instanceState"), "name"))
            .filter(|state| !state.is_empty());
        if held.as_deref() == Some(state) || Instant::now() >= deadline {
            return held;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
