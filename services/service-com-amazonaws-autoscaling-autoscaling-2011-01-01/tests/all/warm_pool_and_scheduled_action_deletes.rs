// SPDX-License-Identifier: CC0-1.0

//! Deleting ONE child of an Auto Scaling group over the awsQuery wire: the warm pool beside it
//! (`DeleteWarmPool`) and one of its scheduled actions (`DeleteScheduledAction`).
//!
//! RULE under test: **a delete addressed at one child of a group removes exactly that child
//! from the collection its own reader reports, leaving the group and every other child of it
//! where they are; a child the group does not hold names nothing to delete, which is refused;
//! and a warm pool that still holds instances is removed only when the request asks for the
//! force delete the model publishes for it — "the warm pool is to be deleted along with all of
//! its associated instances" — so a pool a client warmed is not emptied by a request that did
//! not ask for that.**
//!
//! Why it matters: these are the calls a client (and CloudFormation, which deploys the two as
//! template resources of their own) takes a child down with. A delete that answers success and
//! leaves the child, or that takes a warmed pool's instances out from under a caller who did
//! not ask, is the difference between a teardown and a surprise — and the safety refusal is the
//! only signal a client gets that the pool is still in use.
//!
//! The rows differ along what the rule generalises over: WHICH child is deleted, whether the
//! group holds it, whether the group exists at all, whether the pool holds instances, and
//! whether the request asks for the force delete.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;
use service_com_amazonaws_ec2_amazonec2::state as ec2;

use crate::support::{borrowed, emulator, p, query, tag_value, tag_values, xml_unescape};

/// How often a poll re-reads a warm pool, and how long it keeps trying: the lifecycle span
/// this service owes a transition and the one Amazon EC2 owes, with ample margin for a loaded
/// machine — derived from the services' own spans, so they cannot drift apart.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

fn poll_budget() -> Duration {
    autoscaling::state::LIFECYCLE_TRANSITION.max(ec2::TRANSITION_DELAY) + Duration::from_secs(10)
}

/// The zone the groups of this suite place instances in, in the Region the harness signs for.
const ZONE: &str = "us-west-2a";

/// The launch template the harness seeds for every suite's groups.
const LAUNCH_TEMPLATE: &str = "lt-1";

/// Create one group placed by Availability Zone alone, with the bounds a case needs.
async fn create_group(router: &MultiServiceRouter, name: &str, max_size: i32) {
    let params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", LAUNCH_TEMPLATE),
        p("MinSize", "0"),
        p("MaxSize", max_size.to_string()),
        p("DesiredCapacity", "0"),
        p("AvailabilityZones.member.1", ZONE),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "creating {name:?}: expected 200, got {status} with {body}"
    );
}

/// The `WarmPoolConfiguration` that service reports for `group`, or `None` where it reports no
/// pool at all; the instance ids it reports beside it.
async fn reported_pool(
    case: &str,
    router: &MultiServiceRouter,
    group: &str,
) -> (Option<String>, Vec<String>) {
    let (status, body) = query(
        router,
        "DescribeWarmPool",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeWarmPool({group}) expected 200, got {status} with {body}"
    );
    let configuration = tag_value(&body, "WarmPoolConfiguration");
    let held = (!configuration.is_empty()).then_some(configuration);
    (held, tag_values(&body, "InstanceId"))
}

/// Read `group`'s warm pool until it holds `wanted` instances, or a generous deadline passes —
/// the CONDITION, never a fixed wait.
async fn pool_holding(
    case: &str,
    router: &MultiServiceRouter,
    group: &str,
    wanted: usize,
) -> Vec<String> {
    let deadline = Instant::now() + poll_budget();
    loop {
        let (_, instances) = reported_pool(case, router, group).await;
        if instances.len() == wanted || Instant::now() >= deadline {
            return instances;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The scheduled action names that service reports for `group`, sorted.
async fn reported_actions(case: &str, router: &MultiServiceRouter, group: &str) -> Vec<String> {
    let (status, body) = query(
        router,
        "DescribeScheduledActions",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeScheduledActions({group}) expected 200, got {status} with {body}"
    );
    let mut names = tag_values(&body, "ScheduledActionName");
    names.sort();
    names
}

/// RULE: a delete of one scheduled action removes that action and nothing else; a name the
/// group does not hold — and a group this service holds nothing for — names no action to
/// delete, which is refused with the sentence this service states for a scheduled action it
/// cannot find.
#[tokio::test]
async fn deleting_one_scheduled_action_leaves_every_other_action_of_the_group() {
    let (router, _engine) = emulator();
    let group = "asg-child-delete-actions";
    let other_group = "asg-child-delete-actions-other";
    create_group(&router, group, 2).await;
    create_group(&router, other_group, 2).await;

    // Two actions on the group, and one of the same NAME on another group: a delete addressed
    // at one pair cannot be passing by deleting every action of that name.
    for (holder, action) in [
        (group, "nightly"),
        (group, "weekend"),
        (other_group, "nightly"),
    ] {
        let params = vec![
            p("AutoScalingGroupName", holder),
            p("ScheduledActionName", action),
            p("Recurrence", "0 1 * * *"),
            p("MinSize", "0"),
            p("MaxSize", "2"),
            p("DesiredCapacity", "1"),
        ];
        let (status, body) = query(
            &router,
            "PutScheduledUpdateGroupAction",
            &borrowed(&params),
        )
        .await;
        assert_eq!(
            status, 200,
            "putting {action:?} on {holder:?}: expected 200, got {status} with {body}"
        );
    }

    let case = "one of the two actions of the group";
    let (status, body) = query(
        &router,
        "DeleteScheduledAction",
        &[
            ("AutoScalingGroupName", group),
            ("ScheduledActionName", "nightly"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the delete to be accepted, got {status} with {body}"
    );
    assert_eq!(
        reported_actions(case, &router, group).await,
        vec!["weekend".to_string()],
        "case {case:?}: expected only the other action to survive"
    );
    // The action of the SAME name on the other group is that group's own, and the delete did
    // not touch it.
    assert_eq!(
        reported_actions(case, &router, other_group).await,
        vec!["nightly".to_string()],
        "case {case:?}: expected the other group to keep its own action"
    );

    // A name the group no longer holds and a name it never held each name no action to delete,
    // which is refused for the ACTION; a group this service holds nothing for holds no action
    // either and is refused for the GROUP, which is resolved first.
    let absent_group = "asg-child-delete-absent";
    for (case, holder, action, expected) in [
        (
            "the name just deleted",
            group,
            "nightly",
            autoscaling::state::absent_scheduled_action_message("nightly"),
        ),
        (
            "a name the group never held",
            group,
            "quarterly",
            autoscaling::state::absent_scheduled_action_message("quarterly"),
        ),
        (
            "a group this service holds nothing for",
            absent_group,
            "nightly",
            autoscaling::state::absent_group_message(absent_group),
        ),
    ] {
        let (status, body) = query(
            &router,
            "DeleteScheduledAction",
            &[
                ("AutoScalingGroupName", holder),
                ("ScheduledActionName", action),
            ],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the delete to be refused, got {status} with {body}"
        );
        assert_eq!(
            (
                tag_value(&body, "Code"),
                xml_unescape(&tag_value(&body, "Message"))
            ),
            ("ValidationError".to_string(), expected),
            "case {case:?}: unexpected refusal, got {body}"
        );
    }
    // ...and the action the group does hold survived every refused delete.
    assert_eq!(
        reported_actions("the refused deletes", &router, group).await,
        vec!["weekend".to_string()],
        "case the refused deletes: expected the group to keep its remaining action"
    );
}

/// RULE: a warm pool that still holds instances is deleted only with the force delete, and the
/// refused delete leaves the pool and its instances where they are; a pool that holds none, and
/// a group that holds no pool at all, are deleted without it — and the group outlives its pool.
#[tokio::test]
async fn a_warm_pool_is_deleted_with_its_instances_only_when_the_request_forces_it() {
    let (router, _engine) = emulator();
    let empty = "asg-child-delete-empty-pool";
    let warmed = "asg-child-delete-warmed-pool";
    let poolless = "asg-child-delete-no-pool";
    create_group(&router, empty, 2).await;
    create_group(&router, warmed, 1).await;
    create_group(&router, poolless, 1).await;

    // A pool that prepares NOTHING: its prepared capacity equals the group's desired capacity
    // and its own minimum is zero, so the documented size rule maintains no instance.
    let params = vec![
        p("AutoScalingGroupName", empty),
        p("MinSize", "0"),
        p("MaxGroupPreparedCapacity", "0"),
        p("PoolState", "Stopped"),
    ];
    let (status, body) = query(&router, "PutWarmPool", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "putting the empty pool: expected 200, got {status} with {body}"
    );
    // ...and one that prepares an instance.
    let params = vec![
        p("AutoScalingGroupName", warmed),
        p("MinSize", "1"),
        p("MaxGroupPreparedCapacity", "1"),
        p("PoolState", "Stopped"),
    ];
    let (status, body) = query(&router, "PutWarmPool", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "putting the warmed pool: expected 200, got {status} with {body}"
    );

    // The pool holding no instance is deleted by a request that asks for no force at all, and
    // the group it was beside still answers the read.
    let case = "a pool holding no instance, deleted without the force";
    let (status, body) = query(
        &router,
        "DeleteWarmPool",
        &[("AutoScalingGroupName", empty)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the delete to be accepted, got {status} with {body}"
    );
    assert_eq!(
        reported_pool(case, &router, empty).await,
        (None, Vec::new()),
        "case {case:?}: expected the group to hold no pool"
    );

    // A group that holds no pool is already in the state the delete asks for.
    let case = "a group that holds no pool";
    let (status, body) = query(
        &router,
        "DeleteWarmPool",
        &[("AutoScalingGroupName", poolless)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the delete to be accepted, got {status} with {body}"
    );

    // The pool that HOLDS an instance is not deleted without the force: the refusal is the
    // operation's own `ResourceInUse`, in that service's fixed sentence, and the pool survives
    // it whole.
    let case = "a pool holding an instance, deleted without the force";
    let held = pool_holding(case, &router, warmed, 1).await;
    assert_eq!(
        held.len(),
        1,
        "case {case:?}: expected the pool to have warmed an instance, got {held:?}"
    );
    let (status, body) = query(
        &router,
        "DeleteWarmPool",
        &[("AutoScalingGroupName", warmed)],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected the delete to be refused, got {status} with {body}"
    );
    assert_eq!(
        (
            tag_value(&body, "Code"),
            xml_unescape(&tag_value(&body, "Message"))
        ),
        (
            "ResourceInUse".to_string(),
            autoscaling::state::WARM_POOL_IN_USE_MESSAGE.to_string()
        ),
        "case {case:?}: unexpected refusal, got {body}"
    );
    let (configuration, instances) = reported_pool(case, &router, warmed).await;
    assert!(
        configuration.is_some() && instances == held,
        "case {case:?}: expected the pool and its instances to survive the refusal, got \
         {configuration:?} with {instances:?}"
    );

    // ...and a PUT that would put the pool in a state the model does not publish is refused
    // too, whatever the state SPELLS: `Terminating` names the state this service reports an
    // instance it is taking OUT of the pool in, so a pool stored in it would make a settled
    // instance indistinguishable from one on its way out. The pool keeps the state and the
    // instance it had.
    let case = "a put whose pool state the model does not publish";
    let params = vec![
        p("AutoScalingGroupName", warmed),
        p("PoolState", "Terminating"),
    ];
    let (status, body) = query(&router, "PutWarmPool", &borrowed(&params)).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected the put to be refused, got {status} with {body}"
    );
    assert_eq!(
        tag_value(&body, "Code"),
        "ValidationError",
        "case {case:?}: expected a ValidationError, got {body}"
    );
    let (configuration, instances) = reported_pool(case, &router, warmed).await;
    assert_eq!(
        (
            configuration
                .as_deref()
                .map(|held| tag_value(held, "PoolState"))
                .unwrap_or_default(),
            instances
        ),
        ("Stopped".to_string(), held.clone()),
        "case {case:?}: expected the pool to keep the state and the instance it had, got \
         {configuration:?}"
    );

    // With the force, the same delete is accepted and the pool is gone — instances included.
    let case = "the same pool, deleted with the force";
    let (status, body) = query(
        &router,
        "DeleteWarmPool",
        &[
            ("AutoScalingGroupName", warmed),
            ("ForceDelete", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the forced delete to be accepted, got {status} with {body}"
    );
    assert_eq!(
        reported_pool(case, &router, warmed).await,
        (None, Vec::new()),
        "case {case:?}: expected the pool and its instances to be gone"
    );

    // A group this service holds nothing for has no pool to delete, which is refused for the
    // GROUP — the sentence the warm pool's own writer and reader both state.
    let case = "a group this service holds nothing for";
    let absent = "asg-child-delete-absent-group";
    let (status, body) = query(
        &router,
        "DeleteWarmPool",
        &[("AutoScalingGroupName", absent)],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected the delete to be refused, got {status} with {body}"
    );
    assert_eq!(
        (
            tag_value(&body, "Code"),
            xml_unescape(&tag_value(&body, "Message"))
        ),
        (
            "ValidationError".to_string(),
            autoscaling::state::absent_group_message_as_no_such_group(absent)
        ),
        "case {case:?}: unexpected refusal, got {body}"
    );
}
