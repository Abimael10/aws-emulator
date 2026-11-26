// SPDX-License-Identifier: CC0-1.0

//! WHICH members of a group an instance refresh replaces, end to end over the awsQuery wire.
//!
//! RULE: a refresh replaces exactly the members its preferences admit. Its preferences carry
//! "the behaviors that you want Amazon EC2 Auto Scaling to use if instances that are in
//! Standby state or protected from scale in are found"
//! (`smithy_query(query="doc",shape="StartInstanceRefreshType",member="Preferences")`) — one
//! member per class of member — and each states the same three choices for its own class
//! (`smithy_query(query="doc",shape="RefreshPreferences",member="ScaleInProtectedInstances")`,
//! `…member="StandbyInstances"`): the value that REPLACES them (`Refresh` for a member
//! protected from scale in, `Terminate` for one parked in `Standby`) has the refresh replace
//! that member like any other; `Ignore` has it "ignore" those members "and continue to
//! replace instances that are not protected", so such a member survives the refresh under the
//! instance id it already had and the refresh still succeeds; and `Wait`, "the default", has
//! it "wait … for you to remove scale-in protection" — so the refresh replaces the members it
//! may, holds on that one, and carries on once a caller has cleared the condition.
//!
//! The rows span what the rule generalises over: WHICH class of member the preference governs
//! (protected from scale in, parked in `Standby`), and WHICH of the three behaviours it names
//! for that class (replace, ignore, and the default that waits) — including the pair of
//! values the issue this suite was written for did not name. Every row drives one group of two
//! members through `StartInstanceRefresh` and reads the result where a client reads it:
//! `DescribeAutoScalingGroups` for the ids the group still holds, and
//! `DescribeInstanceRefreshes` for what the refresh did.
//!
//! Every wait here looks AGAIN for the CONDITION it needs under a generous deadline, never for
//! a fixed span, so a loaded or a faster host answers the same.

use std::sync::Arc;
use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;

use crate::support::{
    borrowed, create_launch_template, emulator, list_members, p, query, seed_subnets, tag_value,
    SeededSubnet,
};

// The behaviours either preference names, and the wait the default runs under, taken from the
// service's own spelling of them rather than written out here, so a row drives exactly the
// value the implementation reads (`the_behaviours_are_the_published_value_sets` below pins
// that set against the model's).
use autoscaling::state::refresh_members::{
    BEHAVIOR_IGNORE, BEHAVIOR_WAIT, MEMBER_CONDITION_WAIT, SCALE_IN_PROTECTED_REPLACE,
    STANDBY_REPLACE,
};

/// The `LifecycleState` values this suite reads a member in.
const LIFECYCLE_IN_SERVICE: &str = "InService";
const LIFECYCLE_STANDBY: &str = "Standby";

/// The status reason a refresh that waited its condition out reports, which the operation's
/// own documentation states the reason for: "an instance refresh might fail for several
/// reasons, such as EC2 launch failures, misconfigured health checks, or not ignoring or
/// allowing the termination of instances that are in Standby state or protected from scale
/// in" (`smithy_query(query="operation",operation="StartInstanceRefresh")`).
const SCALE_IN_PROTECTED_FAILURE_REASON: &str =
    "The instance refresh failed because it did not ignore or allow the termination of \
     instances that are protected from scale in.";

/// The statuses a refresh is OVER in, as `InstanceRefresh$Status` names them
/// (`smithy_query(query="shape",shape="InstanceRefreshStatus")`): a refresh in none of them is
/// still running.
const TERMINAL_STATUSES: &[&str] = &[
    "Cancelled",
    "Failed",
    "RollbackFailed",
    "RollbackSuccessful",
    "Successful",
];

/// How long a wait looks again for the condition it needs. Generous on purpose — many times
/// every span this service moves an instance or a refresh over — because what is asserted is
/// that the condition is reached at all, never how fast the host runs.
const POLL_BUDGET: Duration = Duration::from_secs(180);

/// How often it looks again while it waits.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// One member of a group, as a client reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Member {
    instance_id: String,
    lifecycle_state: String,
}

/// Read the members of one group back over the awsQuery wire, in the order it reports them.
async fn members(router: &MultiServiceRouter, case: &str, name: &str) -> Vec<Member> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeAutoScalingGroups for {name:?} expected 200, got {status}: {body}"
    );
    list_members(&body, "Instances")
        .into_iter()
        .map(|member| Member {
            instance_id: tag_value(member, "InstanceId"),
            lifecycle_state: tag_value(member, "LifecycleState"),
        })
        .collect()
}

/// Create one group of two members, placed by subnet.
async fn create_group(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    template: &str,
    subnets: &[SeededSubnet],
) {
    let placement: Vec<&str> = subnets.iter().map(|subnet| subnet.id.as_str()).collect();
    let params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", template),
        p("MinSize", "0"),
        p("MaxSize", "4"),
        p("DesiredCapacity", "2"),
        p("VPCZoneIdentifier", placement.join(",")),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAutoScalingGroup for {name:?} expected 200, got {status}: {body}"
    );
}

/// Look again until the group holds `wanted` members, every one of them in service, and answer
/// them in the order the group reports them: the steady state every row starts from.
async fn settled_members(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    wanted: usize,
) -> Vec<Member> {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let held = members(router, case, name).await;
        let settled = held.len() == wanted
            && held
                .iter()
                .all(|member| member.lifecycle_state == LIFECYCLE_IN_SERVICE);
        if settled || Instant::now() >= deadline {
            assert!(
                settled,
                "case {case:?}: expected {name:?} to settle on {wanted} member(s) in \
                 {LIFECYCLE_IN_SERVICE}, got {held:?}"
            );
            return held;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Record scale-in protection for one member — or take it off again.
async fn set_protection(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    instance_id: &str,
    protected: bool,
) {
    let (status, body) = query(
        router,
        "SetInstanceProtection",
        &[
            ("AutoScalingGroupName", name),
            ("InstanceIds.member.1", instance_id),
            ("ProtectedFromScaleIn", if protected { "true" } else { "false" }),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SetInstanceProtection for {instance_id} expected 200, got {status}: {body}"
    );
}

/// Park one member out of service, taking the group's desired capacity down with it, and look
/// again until the group reports it parked.
async fn park_member(router: &MultiServiceRouter, case: &str, name: &str, instance_id: &str) {
    let (status, body) = query(
        router,
        "EnterStandby",
        &[
            ("AutoScalingGroupName", name),
            ("InstanceIds.member.1", instance_id),
            ("ShouldDecrementDesiredCapacity", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: EnterStandby for {instance_id} expected 200, got {status}: {body}"
    );
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let held = members(router, case, name).await;
        let parked = held.iter().any(|member| {
            member.instance_id == instance_id && member.lifecycle_state == LIFECYCLE_STANDBY
        });
        if parked || Instant::now() >= deadline {
            assert!(
                parked,
                "case {case:?}: expected {instance_id} to be parked in {LIFECYCLE_STANDBY}, \
                 got {held:?}"
            );
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Start one refresh, stating `preference` — a `(member, value)` pair of `RefreshPreferences`
/// — or stating no preference of that kind at all, which is what leaves the member's own
/// documented default in force.
async fn start_refresh(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    preference: Option<(&str, &str)>,
) {
    let mut params = vec![
        p("AutoScalingGroupName", name),
        // No warmup and no ceiling to surge into, so each replacement is the two moves of a
        // refresh replacing in place and nothing else is being waited for.
        p("Preferences.InstanceWarmup", "0"),
        p("Preferences.MinHealthyPercentage", "100"),
    ];
    if let Some((member, value)) = preference {
        params.push(p(&format!("Preferences.{member}"), value));
    }
    let (status, body) = query(router, "StartInstanceRefresh", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: StartInstanceRefresh for {name:?} expected 200, got {status}: {body}"
    );
    assert!(
        !tag_value(&body, "InstanceRefreshId").is_empty(),
        "case {case:?}: the answer must carry the refresh's id, got {body}"
    );
}

/// What the group's one refresh reports: `(status, percentage complete, instances to update)`.
async fn refresh(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String, String) {
    let (status, body) = query(
        router,
        "DescribeInstanceRefreshes",
        &[("AutoScalingGroupName", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeInstanceRefreshes for {name:?} expected 200, got {status}: {body}"
    );
    (
        tag_value(&body, "Status"),
        tag_value(&body, "PercentageComplete"),
        tag_value(&body, "InstancesToUpdate"),
    )
}

/// Look again until the group's refresh has SUCCEEDED, and answer what it then reports.
async fn succeeded_refresh(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> (String, String, String) {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let reported = refresh(router, case, name).await;
        if reported.0 == "Successful" || Instant::now() >= deadline {
            assert_eq!(
                reported.0, "Successful",
                "case {case:?}: expected the refresh of {name:?} to succeed, got {reported:?}"
            );
            return reported;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Look again until `instance_id` is no longer a member of the group, and answer the members it
/// then holds.
async fn once_replaced(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    instance_id: &str,
) -> Vec<Member> {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let held = members(router, case, name).await;
        let gone = held.iter().all(|member| member.instance_id != instance_id);
        if gone || Instant::now() >= deadline {
            assert!(
                gone,
                "case {case:?}: expected {instance_id} to have been replaced in {name:?}, \
                 got {held:?}"
            );
            return held;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One emulator, one launch template and one group of two settled members: what every row here
/// starts from. Answers the router, the engine it is mounted on, the group's name and its two
/// members, in the order the group reports them.
async fn two_member_group(
    case: &str,
    slug: &str,
) -> (
    MultiServiceRouter,
    Arc<engine::InMemoryEngine>,
    String,
    Vec<Member>,
) {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 2);
    let template = format!("lt-refresh-members-{slug}");
    create_launch_template(&router, &template).await;
    let name = format!("asg-refresh-members-{slug}");
    create_group(&router, case, &name, &template, &subnets).await;
    let held = settled_members(&router, case, &name, 2).await;
    (router, engine, name, held)
}

/// Put the group's refresh back in time by `past`, so the wait it is running under has
/// elapsed without a second of wall clock passing: the refresh's own start is what that wait
/// is weighed against, so moving it is how this suite drives the documented hour.
fn rewind_refresh(engine: &Arc<engine::InMemoryEngine>, case: &str, past: Duration) {
    let state = autoscaling::state::service_state(&**engine)
        .expect("the Auto Scaling store is mounted on this engine");
    let mut store = state.lock();
    let refresh = store
        .instance_refreshes
        .last_mut()
        .unwrap_or_else(|| panic!("case {case:?}: the group must hold the refresh it started"));
    refresh.start_time -= past;
}

/// Look again until the group's refresh reports `status`, and answer the whole answer's
/// `(status, status reason)`.
async fn refresh_settles_as(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    status: &str,
) -> (String, String) {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let (_, body) = {
            let (http, body) = query(
                router,
                "DescribeInstanceRefreshes",
                &[("AutoScalingGroupName", name)],
            )
            .await;
            assert_eq!(
                http, 200,
                "case {case:?}: DescribeInstanceRefreshes expected 200, got {http}: {body}"
            );
            (http, body)
        };
        let reported = (tag_value(&body, "Status"), tag_value(&body, "StatusReason"));
        if reported.0 == status || Instant::now() >= deadline {
            assert_eq!(
                reported.0, status,
                "case {case:?}: expected the refresh of {name:?} to report {status:?}, \
                 got {reported:?}"
            );
            return reported;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The behaviours this suite drives are exactly the value sets the model publishes for the two
/// members (`smithy_query(query="shape",shape="ScaleInProtectedInstances")` and
/// `…shape="StandbyInstances"`, as this crate's generated constants carry them) — so the
/// implementation reads one source for the wire values and no row states a value the service
/// does not admit.
#[test]
fn the_behaviours_are_the_published_value_sets() {
    for (member, published, read) in [
        (
            "ScaleInProtectedInstances",
            autoscaling::ops::types::SCALE_IN_PROTECTED_INSTANCES_VALUES,
            [SCALE_IN_PROTECTED_REPLACE, BEHAVIOR_IGNORE, BEHAVIOR_WAIT],
        ),
        (
            "StandbyInstances",
            autoscaling::ops::types::STANDBY_INSTANCES_VALUES,
            [STANDBY_REPLACE, BEHAVIOR_IGNORE, BEHAVIOR_WAIT],
        ),
    ] {
        let mut published: Vec<&str> = published.to_vec();
        published.sort_unstable();
        let mut read: Vec<&str> = read.to_vec();
        read.sort_unstable();
        assert_eq!(
            read, published,
            "case {member:?}: the behaviours the service reads must be the values the model \
             publishes, got {read:?} against {published:?}"
        );
    }
}

/// RULE: `ScaleInProtectedInstances` = `Ignore` — the protected member survives the refresh
/// under the id it already had, the unprotected one is replaced, and the refresh succeeds.
#[tokio::test]
async fn ignore_spares_the_scale_in_protected_member() {
    let case = "ScaleInProtectedInstances=Ignore over a group with one protected member";
    let (router, _engine, name, before) = two_member_group(case, "ignore").await;
    let (plain, protected) = (before[0].clone(), before[1].clone());
    set_protection(&router, case, &name, &protected.instance_id, true).await;

    start_refresh(
        &router,
        case,
        &name,
        Some(("ScaleInProtectedInstances", BEHAVIOR_IGNORE)),
    )
    .await;
    // What it set out to replace is the unprotected member alone: the spared one is not
    // counted against its progress.
    let (_, _, to_update) = refresh(&router, case, &name).await;
    assert_eq!(
        to_update, "1",
        "case {case:?}: an ignored member is not one of the instances the refresh updates, \
         got {to_update:?} left to update"
    );

    let after = succeeded_refresh(&router, case, &name).await;
    assert_eq!(
        (after.1.as_str(), after.2.as_str()),
        ("100", "0"),
        "case {case:?}: a succeeded refresh is complete with nothing left to update, \
         got {after:?}"
    );
    let held = settled_members(&router, case, &name, 2).await;
    assert!(
        held.iter()
            .any(|member| member.instance_id == protected.instance_id),
        "case {case:?}: expected the protected {} to survive the refresh, got {held:?}",
        protected.instance_id
    );
    assert!(
        held.iter()
            .all(|member| member.instance_id != plain.instance_id),
        "case {case:?}: expected the unprotected {} to have been replaced, got {held:?}",
        plain.instance_id
    );
}

/// RULE, the other way round: `ScaleInProtectedInstances` = `Refresh` replaces the protected
/// member like any other — the contrast that makes `Ignore` observable.
#[tokio::test]
async fn refresh_replaces_the_scale_in_protected_member() {
    let case = "ScaleInProtectedInstances=Refresh over a group with one protected member";
    let (router, _engine, name, before) = two_member_group(case, "replace").await;
    let (plain, protected) = (before[0].clone(), before[1].clone());
    set_protection(&router, case, &name, &protected.instance_id, true).await;

    start_refresh(
        &router,
        case,
        &name,
        Some(("ScaleInProtectedInstances", SCALE_IN_PROTECTED_REPLACE)),
    )
    .await;
    let (_, _, to_update) = refresh(&router, case, &name).await;
    assert_eq!(
        to_update, "2",
        "case {case:?}: a refresh that replaces protected members sets out to replace every \
         member, got {to_update:?} left to update"
    );

    succeeded_refresh(&router, case, &name).await;
    let held = settled_members(&router, case, &name, 2).await;
    assert!(
        held.iter().all(|member| member.instance_id != plain.instance_id
            && member.instance_id != protected.instance_id),
        "case {case:?}: expected both {} and the protected {} to have been replaced, \
         got {held:?}",
        plain.instance_id,
        protected.instance_id
    );
}

/// RULE: with no behaviour stated — the default both members document — the refresh replaces
/// the members it may and WAITS on the protected one: it is still running, with that member
/// still in the group, and it carries on and succeeds once a caller removes the protection.
#[tokio::test]
async fn the_default_waits_for_the_protection_to_be_removed() {
    let case = "no ScaleInProtectedInstances stated over a group with one protected member";
    let (router, _engine, name, before) = two_member_group(case, "default").await;
    let (plain, protected) = (before[0].clone(), before[1].clone());
    set_protection(&router, case, &name, &protected.instance_id, true).await;

    start_refresh(&router, case, &name, None).await;
    // The member it may replace IS replaced — so the hold is on the protected one rather than
    // on the refresh as a whole.
    let held = once_replaced(&router, case, &name, &plain.instance_id).await;
    assert!(
        held.iter()
            .any(|member| member.instance_id == protected.instance_id),
        "case {case:?}: expected the protected {} to still be a member while the refresh \
         waits, got {held:?}",
        protected.instance_id
    );
    let waiting = refresh(&router, case, &name).await;
    assert!(
        !TERMINAL_STATUSES.contains(&waiting.0.as_str()),
        "case {case:?}: a refresh waiting for the protection to be removed is still running, \
         got {waiting:?}"
    );

    // Clearing the condition is what lets it carry on, with no further call about the refresh.
    set_protection(&router, case, &name, &protected.instance_id, false).await;
    succeeded_refresh(&router, case, &name).await;
    let after = settled_members(&router, case, &name, 2).await;
    assert!(
        after
            .iter()
            .all(|member| member.instance_id != protected.instance_id),
        "case {case:?}: expected the once-protected {} to be replaced once the protection was \
         removed, got {after:?}",
        protected.instance_id
    );
}

/// RULE, the end of that wait: the default does not wait forever — a refresh whose waited-on
/// condition is never cleared FAILS once the documented wait is over, stating the reason the
/// operation documents, and the member it was waiting on is left exactly where it was.
///
/// The wait is weighed against the refresh's own start, so this row moves that start back past
/// it rather than waiting the hour out: the clock the refresh runs on is driven, never slept
/// through.
#[tokio::test]
async fn the_wait_ends_in_a_failed_refresh() {
    let case = "the default, with the protection never removed";
    let (router, engine, name, before) = two_member_group(case, "waited-out").await;
    let (plain, protected) = (before[0].clone(), before[1].clone());
    set_protection(&router, case, &name, &protected.instance_id, true).await;

    start_refresh(&router, case, &name, None).await;
    once_replaced(&router, case, &name, &plain.instance_id).await;
    // One second past the wait, so the refresh's next look finds it over.
    rewind_refresh(
        &engine,
        case,
        MEMBER_CONDITION_WAIT + Duration::from_secs(1),
    );

    let (status, reason) = refresh_settles_as(&router, case, &name, "Failed").await;
    assert_eq!(
        reason, SCALE_IN_PROTECTED_FAILURE_REASON,
        "case {case:?}: expected the failure to state why, got {status:?} with {reason:?}"
    );
    let held = members(&router, case, &name).await;
    assert!(
        held.iter()
            .any(|member| member.instance_id == protected.instance_id),
        "case {case:?}: expected the protected {} to be left in the group by the failed \
         refresh, got {held:?}",
        protected.instance_id
    );
}

#[tokio::test]
async fn ignore_spares_the_parked_member() {
    let case = "StandbyInstances=Ignore over a group with one parked member";
    let (router, _engine, name, before) = two_member_group(case, "standby-ignore").await;
    let (serving, parked) = (before[0].clone(), before[1].clone());
    park_member(&router, case, &name, &parked.instance_id).await;

    start_refresh(
        &router,
        case,
        &name,
        Some(("StandbyInstances", BEHAVIOR_IGNORE)),
    )
    .await;
    let (_, _, to_update) = refresh(&router, case, &name).await;
    assert_eq!(
        to_update, "1",
        "case {case:?}: an ignored parked member is not one of the instances the refresh \
         updates, got {to_update:?} left to update"
    );

    succeeded_refresh(&router, case, &name).await;
    let held = once_replaced(&router, case, &name, &serving.instance_id).await;
    assert!(
        held.iter().any(|member| member.instance_id == parked.instance_id
            && member.lifecycle_state == LIFECYCLE_STANDBY),
        "case {case:?}: expected the parked {} to be left in {LIFECYCLE_STANDBY}, got {held:?}",
        parked.instance_id
    );
}

/// And its contrast: `StandbyInstances` = `Terminate` has the refresh replace the parked member
/// too, so its id is gone from the group when the refresh succeeds.
#[tokio::test]
async fn terminate_replaces_the_parked_member() {
    let case = "StandbyInstances=Terminate over a group with one parked member";
    let (router, _engine, name, before) = two_member_group(case, "standby-terminate").await;
    let (serving, parked) = (before[0].clone(), before[1].clone());
    park_member(&router, case, &name, &parked.instance_id).await;

    start_refresh(
        &router,
        case,
        &name,
        Some(("StandbyInstances", STANDBY_REPLACE)),
    )
    .await;
    let (_, _, to_update) = refresh(&router, case, &name).await;
    assert_eq!(
        to_update, "2",
        "case {case:?}: a refresh that terminates parked members sets out to replace them \
         too, got {to_update:?} left to update"
    );

    succeeded_refresh(&router, case, &name).await;
    let held = once_replaced(&router, case, &name, &parked.instance_id).await;
    assert!(
        held.iter()
            .all(|member| member.instance_id != serving.instance_id),
        "case {case:?}: expected the member in service, {}, to have been replaced as well, \
         got {held:?}",
        serving.instance_id
    );
}
