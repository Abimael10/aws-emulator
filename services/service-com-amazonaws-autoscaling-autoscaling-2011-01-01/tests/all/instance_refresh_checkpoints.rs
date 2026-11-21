// SPDX-License-Identifier: CC0-1.0

//! The checkpoints an instance refresh holds at.
//!
//! The rule under test: a refresh whose preferences state checkpoint thresholds HOLDS at
//! each threshold its progress reaches — still replacing, so still reported `InProgress`
//! at the percentage it reached, with the instances it has not replaced yet still held by
//! the group — for the delay it runs under, and carries on only once that delay has
//! passed. A refresh that states no threshold has no checkpoint to hold at and runs
//! straight through.
//!
//! The tables below span what that rule generalises over: how many instances the group
//! holds (one, two, four), where the thresholds sit (a single `100`, a pair, three of
//! them), and the delay stated for them (none, which the model documents an hour for, and
//! the seconds a caller states). The first threshold a refresh of a given size reaches is
//! the row's own, so the percentage each case is pinned at is what its replacements
//! actually came to.
//!
//! Every wait here is a wait for the CONDITION: each look is one more call to
//! `DescribeInstanceRefreshes`, and a loop ends when the service reports what the case is
//! about, never when a span of wall clock has gone by. The one span a case does state is
//! the limit it tests — the delay a checkpoint holds for — and every other budget here is
//! generous, so a slow or loaded host answers the same as a fast one.

use std::time::{Duration, Instant};

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;

use crate::support::{
    borrowed, list_members, new_engine, p, query, router_with, tag_value, tag_values,
};

/// The launch template the groups in this suite launch from, one the harness puts in the
/// account's own Amazon EC2 store.
const LAUNCH_TEMPLATE: &str = "lt-refresh";

/// The zone the groups in this suite place their instances in.
const ZONE: &str = "us-west-2a";

/// How often a wait looks again. An interval between looks, not a budget.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How long a wait keeps looking before it reports the condition never held. Generous
/// against every span this service owes.
const PATIENCE: Duration = Duration::from_secs(180);

/// The delay a case states when it means "longer than this refresh would take to finish
/// without a checkpoint": the hold the case asserts is the limit under test, and the looks
/// that assert it span a few of the service's own replacement spans, so the value is far
/// above those.
const HELD_DELAY_SECONDS: i32 = 60;

/// The delay a case states when it means "a few seconds": a hold the case waits out.
const SHORT_DELAY_SECONDS: i32 = 3;

/// `RefreshPreferences$CheckpointDelay` when the request states thresholds and no delay:
/// "if you specify a value for `CheckpointPercentages` and not for `CheckpointDelay`, the
/// `CheckpointDelay` defaults to `3600` (1 hour)"
/// (`smithy_query(query="doc",shape="RefreshPreferences",member="CheckpointDelay")`).
/// Written out here rather than read from the service, so the assertion is against the
/// documented value.
const DOCUMENTED_DEFAULT_DELAY: &str = "3600";

/// How many further looks a case takes while it asserts a refresh is held at its
/// checkpoint. Each look is a call, and the looks are spaced by the service's own
/// replacement span, so together they cover more than a refresh with no checkpoint at all
/// would need to replace every instance of these groups and end.
const HOLD_LOOKS: u32 = 4;

/// What `DescribeInstanceRefreshes` reports for the newest refresh of a group.
#[derive(Debug, Clone, Default)]
struct ReportedRefresh {
    instance_refresh_id: String,
    status: String,
    percentage_complete: String,
    instances_to_update: String,
    end_time: String,
    checkpoint_delay: String,
    checkpoint_percentages: Vec<String>,
}

/// Create one group holding `capacity` instances and wait until it holds them in service.
async fn create_group(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
    capacity: i32,
) {
    let desired = capacity.to_string();
    let params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", LAUNCH_TEMPLATE),
        p("MinSize", "0"),
        p("MaxSize", "8"),
        p("DesiredCapacity", desired.as_str()),
        p("AvailabilityZones.member.1", ZONE),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "CreateAutoScalingGroup for {name} expected 200, got {status}: {body}"
    );
    let wanted = usize::try_from(capacity).expect("a capacity is a count");
    let deadline = Instant::now() + PATIENCE;
    loop {
        let states = group_lifecycle_states(router, name).await;
        if states.iter().filter(|state| *state == "InService").count() == wanted {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "group {name}: expected {wanted} instances in service, got {states:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The lifecycle states one group reports for the instances it holds.
async fn group_lifecycle_states(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
) -> Vec<String> {
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
    tag_values(&body, "LifecycleState")
}

/// The instance ids one group reports.
async fn group_instance_ids(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
) -> Vec<String> {
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
    tag_values(&body, "InstanceId")
}

/// The request parameters that state `thresholds` as `CheckpointPercentages` and `delay`
/// as `CheckpointDelay`, the way a client sends them.
fn checkpoint_params(thresholds: &[i32], delay: Option<i32>) -> Vec<(String, String)> {
    let mut params: Vec<(String, String)> = thresholds
        .iter()
        .enumerate()
        .map(|(index, threshold)| {
            p(
                &format!("Preferences.CheckpointPercentages.member.{}", index + 1),
                threshold.to_string(),
            )
        })
        .collect();
    if let Some(delay) = delay {
        params.push(p("Preferences.CheckpointDelay", delay.to_string()));
    }
    params
}

/// Start one refresh on `name` with the preferences `preferences` states, and answer the
/// id the service tracks it by.
async fn start_refresh(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
    preferences: &[(String, String)],
) -> String {
    let mut params = vec![p("AutoScalingGroupName", name)];
    params.extend_from_slice(preferences);
    let (status, body) = query(router, "StartInstanceRefresh", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "StartInstanceRefresh for {name} with {preferences:?} expected 200, got {status}: {body}"
    );
    let id = tag_value(&body, "InstanceRefreshId");
    assert!(!id.is_empty(), "a started refresh carries its id, got {body}");
    id
}

/// Describe one group's refreshes and read the newest one out of the answer.
async fn newest_refresh(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
) -> ReportedRefresh {
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
        instance_refresh_id: tag_value(&body, "InstanceRefreshId"),
        status: tag_value(&body, "Status"),
        percentage_complete: tag_value(&body, "PercentageComplete"),
        instances_to_update: tag_value(&body, "InstancesToUpdate"),
        end_time: tag_value(&body, "EndTime"),
        checkpoint_delay: tag_value(&body, "CheckpointDelay"),
        checkpoint_percentages: list_members(&body, "CheckpointPercentages")
            .into_iter()
            .map(::std::string::ToString::to_string)
            .collect(),
    }
}

/// Keep reading one group's newest refresh until it reports one of `wanted` as its status.
async fn until_status(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
    case: &str,
    wanted: &[&str],
) -> ReportedRefresh {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let refresh = newest_refresh(router, name).await;
        if wanted.contains(&refresh.status.as_str()) {
            return refresh;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the refresh to be reported one of {wanted:?}, got \
             {refresh:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Keep reading one group's newest refresh until it reports `wanted` as its percentage
/// complete.
async fn until_percentage(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
    case: &str,
    wanted: i32,
) -> ReportedRefresh {
    let wanted = wanted.to_string();
    let deadline = Instant::now() + PATIENCE;
    loop {
        let refresh = newest_refresh(router, name).await;
        if refresh.percentage_complete == wanted {
            return refresh;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the refresh to report {wanted}% complete, got {refresh:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row of the table of refreshes that are held at a checkpoint.
struct Held {
    case: &'static str,
    /// How many instances the group holds when the refresh starts.
    capacity: i32,
    /// The thresholds the request states.
    thresholds: &'static [i32],
    /// The delay the request states, or `None` to leave it to the documented default.
    delay: Option<i32>,
    /// The percentage the refresh's replacements first carry it to, which is the
    /// threshold it is then held at.
    held_at: i32,
    /// How many instances are left to update while it is held there.
    left_to_update: i32,
}

/// Every row: the refresh reaches the row's first threshold and then STAYS there — still
/// `InProgress`, still at that percentage, still owing the instances it has not replaced
/// — across looks spanning several of the service's own replacement spans, which is
/// longer than the same refresh without a checkpoint would take to finish.
#[tokio::test]
async fn a_checkpoint_holds_a_refresh_at_the_percentage_it_reached() {
    let rows = [
        Held {
            case: "one instance and a single threshold at 100, the delay a caller states",
            capacity: 1,
            thresholds: &[100],
            delay: Some(HELD_DELAY_SECONDS),
            held_at: 100,
            left_to_update: 0,
        },
        Held {
            case: "two instances and a threshold halfway, the delay a caller states",
            capacity: 2,
            thresholds: &[50, 100],
            delay: Some(HELD_DELAY_SECONDS),
            held_at: 50,
            left_to_update: 1,
        },
        Held {
            case: "four instances and three thresholds, the delay a caller states",
            capacity: 4,
            thresholds: &[25, 50, 100],
            delay: Some(HELD_DELAY_SECONDS),
            held_at: 25,
            left_to_update: 3,
        },
        Held {
            case: "two instances and thresholds stated with no delay, which defaults to an hour",
            capacity: 2,
            thresholds: &[50, 100],
            delay: None,
            held_at: 50,
            left_to_update: 1,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        // One emulator per row: each row is its own account, so the ids of one row never
        // explain another's.
        let router = router_with(new_engine());
        let name = format!("asg-checkpoint-held-{index}");
        create_group(&router, &name, row.capacity).await;
        let before = group_instance_ids(&router, &name).await;

        let preferences = checkpoint_params(row.thresholds, row.delay);
        let started = start_refresh(&router, &name, &preferences).await;
        let reached = until_percentage(&router, &name, row.case, row.held_at).await;
        assert_eq!(
            reached.instance_refresh_id, started,
            "case {:?}: the group reports the refresh it was given, got {reached:?}",
            row.case
        );

        // The hold: every further look reports the same checkpoint, and the looks are
        // spaced by the span the service takes per replacement — so a refresh that had
        // not been held would have moved on, and finished, several looks ago.
        for look in 0..HOLD_LOOKS {
            tokio::time::sleep(autoscaling::state::INSTANCE_REFRESH_REPLACEMENT).await;
            let refresh = newest_refresh(&router, &name).await;
            assert_eq!(
                refresh.status, "InProgress",
                "case {:?}: look {look}: a refresh held at a checkpoint is still replacing, \
                 got {refresh:?}",
                row.case
            );
            assert_eq!(
                refresh.percentage_complete,
                row.held_at.to_string(),
                "case {:?}: look {look}: expected the refresh pinned at {}%, got {refresh:?}",
                row.case,
                row.held_at
            );
            assert_eq!(
                refresh.instances_to_update,
                row.left_to_update.to_string(),
                "case {:?}: look {look}: expected {} instances still to update, got {refresh:?}",
                row.case,
                row.left_to_update
            );
            assert!(
                refresh.end_time.is_empty(),
                "case {:?}: look {look}: a held refresh has not ended, got {refresh:?}",
                row.case
            );
        }

        // The hold is backed by the group: the instances the refresh has not replaced yet
        // are the ones it still holds from before the refresh started.
        let held = group_instance_ids(&router, &name).await;
        let kept = held.iter().filter(|id| before.contains(id)).count();
        assert_eq!(
            kept,
            usize::try_from(row.left_to_update).expect("a count of instances"),
            "case {:?}: expected {} of the original instances {before:?} still held while the \
             refresh waits at its checkpoint, got {held:?}",
            row.case,
            row.left_to_update
        );
    }
}

/// One row of the table of refreshes that run to the end.
struct RunsOn {
    case: &'static str,
    capacity: i32,
    thresholds: &'static [i32],
    delay: Option<i32>,
}

/// Every row: a refresh whose checkpoint delay is a few seconds — and a refresh that
/// states no checkpoint at all — carries on past every threshold and succeeds, having
/// replaced every instance of its group.
#[tokio::test]
async fn a_refresh_continues_once_its_checkpoint_delay_has_passed() {
    let rows = [
        RunsOn {
            case: "one instance and a single threshold at 100",
            capacity: 1,
            thresholds: &[100],
            delay: Some(SHORT_DELAY_SECONDS),
        },
        RunsOn {
            case: "two instances and a threshold at each half",
            capacity: 2,
            thresholds: &[50, 100],
            delay: Some(SHORT_DELAY_SECONDS),
        },
        RunsOn {
            case: "three thresholds a four-instance refresh passes in turn",
            capacity: 4,
            thresholds: &[25, 50, 100],
            delay: Some(SHORT_DELAY_SECONDS),
        },
        RunsOn {
            case: "no threshold at all, so no checkpoint to hold at",
            capacity: 2,
            thresholds: &[],
            delay: None,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let router = router_with(new_engine());
        let name = format!("asg-checkpoint-continues-{index}");
        create_group(&router, &name, row.capacity).await;
        let before = group_instance_ids(&router, &name).await;

        let preferences = checkpoint_params(row.thresholds, row.delay);
        let started = start_refresh(&router, &name, &preferences).await;
        let finished = until_status(&router, &name, row.case, &["Successful"]).await;
        assert_eq!(
            finished.instance_refresh_id, started,
            "case {:?}: the refresh keeps the id it was started under, got {finished:?}",
            row.case
        );
        assert_eq!(
            finished.percentage_complete, "100",
            "case {:?}: a succeeded refresh is complete, got {finished:?}",
            row.case
        );

        let after = group_instance_ids(&router, &name).await;
        let kept: Vec<&String> = after.iter().filter(|id| before.contains(id)).collect();
        assert!(
            kept.is_empty(),
            "case {:?}: every instance the refresh replaced is gone, {kept:?} of {before:?} are \
             still held",
            row.case
        );
    }
}

/// One row of the preference-reporting table.
struct Reported {
    case: &'static str,
    thresholds: &'static [i32],
    delay: Option<i32>,
    /// The `CheckpointDelay` the refresh must report back, or `""` for a refresh that has
    /// no checkpoint to hold at and so no delay of its own.
    expected_delay: &'static str,
}

/// Every row: a refresh reports the thresholds it runs under and the delay it holds at
/// them for — the seconds the request stated, or the hour the model documents for a
/// request that stated thresholds and no delay. A request that stated neither reports no
/// delay at all.
#[tokio::test]
async fn a_refresh_reports_the_checkpoint_delay_it_runs_under() {
    let rows = [
        Reported {
            case: "the seconds a caller states",
            thresholds: &[20, 100],
            delay: Some(120),
            expected_delay: "120",
        },
        Reported {
            case: "thresholds stated with no delay",
            thresholds: &[50],
            delay: None,
            expected_delay: DOCUMENTED_DEFAULT_DELAY,
        },
        Reported {
            case: "no thresholds and no delay",
            thresholds: &[],
            delay: None,
            expected_delay: "",
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let router = router_with(new_engine());
        let name = format!("asg-checkpoint-reported-{index}");
        // A group holding no instance: the preferences a refresh runs under are the
        // request's own, whatever the refresh then has to replace.
        create_group(&router, &name, 0).await;

        let preferences = checkpoint_params(row.thresholds, row.delay);
        start_refresh(&router, &name, &preferences).await;
        let reported = newest_refresh(&router, &name).await;
        assert_eq!(
            reported.checkpoint_delay, row.expected_delay,
            "case {:?}: expected CheckpointDelay {:?}, got {reported:?}",
            row.case, row.expected_delay
        );
        let thresholds: Vec<String> = row
            .thresholds
            .iter()
            .map(::std::string::ToString::to_string)
            .collect();
        assert_eq!(
            reported.checkpoint_percentages, thresholds,
            "case {:?}: expected the thresholds {thresholds:?} reported back, got {reported:?}",
            row.case
        );
    }
}
