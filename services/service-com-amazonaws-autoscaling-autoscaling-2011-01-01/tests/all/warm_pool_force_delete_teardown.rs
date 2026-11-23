// SPDX-License-Identifier: CC0-1.0

//! Taking a warm pool down over the awsQuery wire, and what the teardown leaves the group
//! able to do.
//!
//! RULE under test: **`DeleteWarmPool` is answered from the pool's own state — a pool that
//! still holds instances is refused with the modelled `ResourceInUseFault` (HTTP 400, the
//! wire code `ResourceInUse`) and that refusal's one fixed sentence, whatever the pool holds
//! and however the request spells "no force", leaving the pool and its instances where they
//! are; the same call carrying the force delete the model publishes for it is accepted with
//! an empty result, the pool and the Amazon EC2 instances it held are taken down where they
//! live, and the teardown holds nothing on the group — another operation on it, such as
//! starting an instance refresh, is accepted while the instances are still settling.**
//!
//! Why it matters: these are the only two answers a client can branch on. A call that is
//! neither the documented safety refusal nor the documented success (a 501, say) is a code no
//! SDK models, so a client can neither protect the instances it warmed nor tear the pool
//! down; and a teardown that left the group locked would turn the force delete into a stall
//! for every call that follows it.
//!
//! The rows differ along what the rule generalises over: how many instances the pool holds
//! (so the fixed sentence is shown to carry no count), and HOW the request spells the boolean
//! — the member left out, either word, either numeral — on both the refused and the accepted
//! side.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;
use service_com_amazonaws_ec2_amazonec2::state as ec2;

use crate::support::{
    borrowed, ec2_query, element_body, emulator, p, query, tag_value, tag_values, xml_unescape,
};

/// How often a poll re-reads, and how long it keeps trying: the lifecycle span this service
/// owes a transition and the one Amazon EC2 owes, with ample margin for a loaded machine —
/// derived from the services' own spans, so they cannot drift apart.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

fn poll_budget() -> Duration {
    autoscaling::state::LIFECYCLE_TRANSITION.max(ec2::TRANSITION_DELAY) + Duration::from_secs(10)
}

/// The zone the groups of this suite place instances in, in the Region the harness signs for.
const ZONE: &str = "us-west-2a";

/// The launch template the harness seeds for every suite's groups.
const LAUNCH_TEMPLATE: &str = "lt-1";

/// Every spelling of a boolean that says NO force: the member left out of the request
/// altogether, and the two spellings of false an XML Schema boolean's lexical space admits
/// (the suite `unreadable_argument_values` states that space for this wire).
const UNFORCED: &[(&str, Option<&str>)] = &[
    ("the member left out", None),
    ("the word false", Some("false")),
    ("the numeral zero", Some("0")),
];

/// ...and the spellings that DO ask for the force delete.
const FORCED: &[(&str, &str)] = &[("the word true", "true"), ("the numeral one", "1")];

/// Create one group placed by Availability Zone alone, holding no instances of its own, with
/// room for `max_size` warmed ones beside it.
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

/// Put a warm pool on `group` that maintains exactly `size` instances, and answer once the
/// pool reports that many.
async fn warm_pool_holding(
    case: &str,
    router: &MultiServiceRouter,
    group: &str,
    size: i32,
) -> Vec<String> {
    let params = vec![
        p("AutoScalingGroupName", group),
        p("MinSize", size.to_string()),
        p("MaxGroupPreparedCapacity", size.to_string()),
        p("PoolState", "Stopped"),
    ];
    let (status, body) = query(router, "PutWarmPool", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutWarmPool({group}) expected 200, got {status} with {body}"
    );
    let wanted = usize::try_from(size).expect("a non-negative pool size");
    let deadline = Instant::now() + poll_budget();
    loop {
        let (_, instances) = reported_pool(case, router, group).await;
        if instances.len() == wanted {
            return instances;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the pool of {group} to warm {wanted} instance(s), it holds \
             {instances:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `WarmPoolConfiguration` the service reports for `group` (`None` where it reports no
/// pool at all) and the instance ids it reports beside it.
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

/// `DeleteWarmPool` for `group`, with the `ForceDelete` spelling `force` (or without the
/// member when it is `None`).
async fn delete_warm_pool(
    router: &MultiServiceRouter,
    group: &str,
    force: Option<&str>,
) -> (u16, String) {
    let mut params = vec![p("AutoScalingGroupName", group)];
    if let Some(force) = force {
        params.push(p("ForceDelete", force));
    }
    query(router, "DeleteWarmPool", &borrowed(&params)).await
}

/// What Amazon EC2 reports one instance's state as, read through that service's own
/// `DescribeInstances` — the service that owns the instance is what says it is gone.
async fn ec2_instance_state(router: &MultiServiceRouter, instance_id: &str) -> Option<String> {
    let (status, body) =
        ec2_query(router, "DescribeInstances", &[p("InstanceId.1", instance_id)]).await;
    if status != 200 {
        return None;
    }
    let state = tag_value(&tag_value(&body, "instanceState"), "name");
    (!state.is_empty()).then_some(state)
}

/// Wait until Amazon EC2 reports every id in `instance_ids` terminated, or a generous
/// deadline passes; answers the last states it reported.
async fn ec2_instances_terminated(
    router: &MultiServiceRouter,
    instance_ids: &[String],
) -> Vec<Option<String>> {
    let deadline = Instant::now() + poll_budget();
    loop {
        let mut states = Vec::with_capacity(instance_ids.len());
        for instance_id in instance_ids {
            states.push(ec2_instance_state(router, instance_id).await);
        }
        let settled = states
            .iter()
            .all(|state| state.as_deref() == Some("terminated"));
        if settled || Instant::now() >= deadline {
            return states;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: a pool that still holds instances is refused without the force, with one fixed
/// sentence that carries no part of the request — whatever the pool holds and however the
/// request spells "no force" — and the pool survives every refusal whole.
#[tokio::test]
async fn a_pool_holding_instances_is_refused_without_the_force_whatever_it_holds() {
    let (router, _engine) = emulator();

    // The rows differ in how many instances the pool holds: one and two, so a sentence that
    // carried the count could not be the same in both.
    for size in [1, 2] {
        let group = format!("asg-force-delete-refusal-{size}");
        let case = format!("a pool holding {size} instance(s)");
        create_group(&router, &group, size).await;
        let held = warm_pool_holding(&case, &router, &group, size).await;

        for (spelling, force) in UNFORCED {
            let case = format!("{case}, deleted with {spelling}");
            let (status, body) = delete_warm_pool(&router, &group, *force).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected the delete to be refused, got {status} with {body}"
            );
            let (code, message) = (
                tag_value(&body, "Code"),
                xml_unescape(&tag_value(&body, "Message")),
            );
            assert_eq!(
                (code.as_str(), message.as_str()),
                (
                    "ResourceInUse",
                    autoscaling::state::WARM_POOL_IN_USE_MESSAGE
                ),
                "case {case:?}: unexpected refusal, got {body}"
            );
            // The sentence is entirely fixed text: nothing of the request — not the group it
            // names, not the number of instances the pool holds — appears in it.
            assert!(
                !message.contains(&group) && !message.contains(&size.to_string()),
                "case {case:?}: the refusal must carry no part of the request, got {message:?}"
            );
            // ...and the pool the refusal protected is still there, instances included.
            let (configuration, instances) = reported_pool(&case, &router, &group).await;
            assert!(
                configuration.is_some() && instances == held,
                "case {case:?}: expected the pool and {held:?} to survive, got {configuration:?} \
                 with {instances:?}"
            );
        }
    }
}

/// RULE: the force delete is accepted with an empty result however it is spelled, takes the
/// pool's Amazon EC2 instances down where they live, and leaves the group free for the next
/// call — an instance refresh started while those instances are still settling is accepted
/// and reported in a status the model publishes.
#[tokio::test]
async fn the_forced_delete_tears_the_pool_down_and_leaves_the_group_refreshable() {
    let (router, _engine) = emulator();

    // Each row pairs a spelling of the force with a pool size, so the accepted side is shown
    // over both of the dimensions the refused side above spans.
    for (row, (spelling, force)) in FORCED.iter().enumerate() {
        let size = i32::try_from(row).expect("a small row index") + 1;
        let group = format!("asg-force-delete-teardown-{size}");
        let case = format!("a pool holding {size} instance(s), deleted with {spelling}");
        create_group(&router, &group, size).await;
        let held = warm_pool_holding(&case, &router, &group, size).await;

        let (status, body) = delete_warm_pool(&router, &group, Some(force)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the forced delete to be accepted, got {status} with {body}"
        );
        // The operation's output carries no member, so its result element carries none either.
        assert!(
            body.contains("DeleteWarmPoolResult")
                && element_body(&body, "DeleteWarmPoolResult")
                    .unwrap_or_default()
                    .trim()
                    .is_empty(),
            "case {case:?}: expected an empty DeleteWarmPoolResult, got {body}"
        );
        // The pool is gone from the group's own reader at once: the record the delete removed
        // is what a later caller reads.
        let (configuration, instances) = reported_pool(&case, &router, &group).await;
        assert_eq!(
            (configuration, instances),
            (None, Vec::new()),
            "case {case:?}: expected the group to report no pool and no warmed instance"
        );

        // The teardown holds nothing on the group: a refresh started while the pool's
        // instances are still settling is ACCEPTED, and tracked like any other.
        let (status, body) = query(
            &router,
            "StartInstanceRefresh",
            &[("AutoScalingGroupName", &group)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the refresh to be accepted during the teardown, got {status} \
             with {body}"
        );
        let refresh_id = tag_value(&body, "InstanceRefreshId");
        assert!(
            !refresh_id.is_empty(),
            "case {case:?}: expected the refresh to answer an id, got {body}"
        );
        let (status, body) = query(
            &router,
            "DescribeInstanceRefreshes",
            &[("AutoScalingGroupName", &group)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeInstanceRefreshes({group}) expected 200, got {status} with \
             {body}"
        );
        assert_eq!(
            tag_value(&body, "InstanceRefreshId"),
            refresh_id,
            "case {case:?}: expected the group to report the refresh it just accepted, got {body}"
        );
        // The status is one the model publishes for a refresh
        // (`smithy_query(query="shape",shape="InstanceRefreshStatus")`, the value set this
        // crate holds that enum's members in).
        let reported_status = tag_value(&body, "Status");
        assert!(
            autoscaling::ops::types::INSTANCE_REFRESH_STATUS_VALUES
                .contains(&reported_status.as_str()),
            "case {case:?}: expected a published refresh status, got {reported_status:?} from \
             {body}"
        );

        // ...and the instances the pool held are shut down where they live: Amazon EC2's own
        // reader is what says so, on its own clock.
        let states = ec2_instances_terminated(&router, &held).await;
        assert!(
            states
                .iter()
                .all(|state| state.as_deref() == Some("terminated")),
            "case {case:?}: expected Amazon EC2 to terminate {held:?}, it reports {states:?}"
        );
    }
}
