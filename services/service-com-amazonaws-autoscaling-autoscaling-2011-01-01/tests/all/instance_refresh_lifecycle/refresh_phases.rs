// SPDX-License-Identifier: CC0-1.0

//! The phases an instance refresh moves through before it is complete.
//!
//! The rule under test: a refresh REPORTS the phase it is actually in. It is created in a
//! non-terminal status and reaches a terminal one only once every step it owes has been
//! taken — so while it is replacing instances it is reported `Pending`/`InProgress`, and
//! where the request asked it to wait at the end it is reported `Baking` until that wait
//! has passed. A refresh in any of those phases is the group's ACTIVE refresh: a second
//! one cannot start on the group, and a cancellation acts on that one and drives it to
//! `Cancelled`.
//!
//! The tables below span what that rule generalises over: how many instances the group
//! holds (none, one, two, three), the warmup the refresh waits after each replacement
//! (none stated, an explicit zero, the seconds a caller states) and the wait it takes at
//! the end (none stated, each published bound of `BakeTime`, a value between them, and
//! one past the maximum).
//!
//! Every wait here is a wait for the CONDITION: each look is one more call to
//! `DescribeInstanceRefreshes`, and the loop ends when the status it reports is the one
//! the case is about, never when a span of wall clock has gone by.

use std::time::{Duration, Instant};

use crate::support::{
    borrowed, new_engine, p, query, router_with, tag_value, tag_values, xml_unescape,
};

/// The launch template the groups in this suite launch from, one the harness puts in the
/// account's own Amazon EC2 store.
const LAUNCH_TEMPLATE: &str = "lt-refresh";

/// The zone the groups in this suite place their instances in.
const ZONE: &str = "us-west-2a";

/// How often a wait looks again. An interval between looks, not a budget: a wait here
/// ends when the service reports the status the case is about.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How long a wait keeps looking before it reports the condition never held. Generous
/// against the spans this service owes, so a slow or loaded host answers the same as a
/// fast one.
const PATIENCE: Duration = Duration::from_secs(120);

/// The statuses that mean a refresh is still replacing instances: "`Pending` - The
/// request was created, but the instance refresh has not started", "`InProgress` - An
/// instance refresh is in progress"
/// (`smithy_query(query="doc",shape="InstanceRefresh",member="Status")`).
const RUNNING: [&str; 2] = ["Pending", "InProgress"];

/// The bake time a case states when it means "a few seconds": the value is the caller's
/// own and the service waits exactly it, so a case that asserts the wait states a short
/// one.
const SHORT_BAKE_SECONDS: &str = "4";

/// The published bounds of `RefreshPreferences$BakeTime`, which the model states as its
/// range (`smithy_query(query="shape",shape="BakeTime")`: min 0, max 172800).
const BAKE_TIME_MINIMUM: i64 = 0;
const BAKE_TIME_MAXIMUM: i64 = 172_800;

/// What `DescribeInstanceRefreshes` reports for the newest refresh of a group.
#[derive(Debug, Clone, Default)]
struct ReportedRefresh {
    instance_refresh_id: String,
    status: String,
    percentage_complete: String,
    instances_to_update: String,
    end_time: String,
    bake_time: String,
}

/// Create one group holding `capacity` instances and wait until it holds them in service.
///
/// `version` pins the launch template version the group launches from, for a case whose
/// subject needs the configuration before a refresh to be pinned; `None` leaves it to the
/// default, as a group that names only a template name does.
async fn create_group(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
    capacity: i32,
    version: Option<&str>,
) {
    let desired = capacity.to_string();
    let mut params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", LAUNCH_TEMPLATE),
        p("MinSize", "0"),
        p("MaxSize", "8"),
        p("DesiredCapacity", desired.as_str()),
        p("AvailabilityZones.member.1", ZONE),
    ];
    if let Some(version) = version {
        params.push(p("LaunchTemplate.Version", version));
    }
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "CreateAutoScalingGroup for {name} expected 200, got {status}: {body}"
    );
    let wanted = usize::try_from(capacity).expect("a capacity is a count");
    let deadline = Instant::now() + PATIENCE;
    loop {
        let states = group_lifecycle_states(router, name).await;
        let in_service = states.iter().filter(|state| *state == "InService").count();
        if in_service == wanted {
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
    assert!(
        !id.is_empty(),
        "a started refresh carries its id, got {body}"
    );
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
        bake_time: tag_value(&body, "BakeTime"),
    }
}

/// What looking for a status reported back: the refresh as it was reported when the wait
/// ended, and every status seen on the way there.
#[derive(Debug)]
struct Waited {
    refresh: ReportedRefresh,
    seen: Vec<String>,
}

/// Keep reading one group's newest refresh until it reports one of `wanted`.
///
/// The wait ends on the CONDITION. A deadline is here only so a refresh that never gets
/// there fails with what it was reporting instead, and it is far longer than any step
/// this service owes.
async fn until_status(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
    case: &str,
    wanted: &[&str],
) -> Waited {
    let deadline = Instant::now() + PATIENCE;
    let mut seen: Vec<String> = Vec::new();
    loop {
        let refresh = newest_refresh(router, name).await;
        if !seen.contains(&refresh.status) {
            seen.push(refresh.status.clone());
        }
        if wanted.contains(&refresh.status.as_str()) {
            return Waited { refresh, seen };
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the refresh to be reported one of {wanted:?}, \
             got {refresh:?} after seeing {seen:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Assert that starting a second refresh on `name` is refused because the group already
/// has an active one.
async fn second_refresh_is_refused(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
    case: &str,
) {
    let (status, body) = query(
        router,
        "StartInstanceRefresh",
        &[("AutoScalingGroupName", name)],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: a group with an active refresh must refuse a second one, \
         got {status}: {body}"
    );
    assert_eq!(
        tag_value(&body, "Code"),
        "InstanceRefreshInProgress",
        "case {case:?}: expected the InstanceRefreshInProgress code, got {body}"
    );
}

/// The sentence a cancellation is refused with when the group has no refresh a
/// cancellation could act on: fixed text apart from the group name, which comes from the
/// request.
fn no_active_refresh_message(name: &str) -> String {
    format!(
        "No in progress, baking, pending, or cancelling Instance Refresh found for Auto \
         Scaling group {name}."
    )
}

/// One row of the replacement table.
struct Replacing {
    case: &'static str,
    /// How many instances the group holds when the refresh starts.
    capacity: i32,
    /// The `InstanceWarmup` the request states, or `None` to leave it to the default the
    /// model documents.
    warmup: Option<&'static str>,
}

/// Every row: the refresh is reported running from the first read, refuses a second
/// refresh while it runs, and reaches `Successful` only once it has actually replaced the
/// group's instances.
#[tokio::test]
async fn a_refresh_is_reported_running_until_it_has_replaced_the_groups_instances() {
    let rows = [
        Replacing {
            case: "a group holding no instance, the warmup left to the default",
            capacity: 0,
            warmup: None,
        },
        Replacing {
            case: "one instance, the warmup left to the default",
            capacity: 1,
            warmup: None,
        },
        Replacing {
            case: "one instance, an explicit zero warmup",
            capacity: 1,
            warmup: Some("0"),
        },
        Replacing {
            case: "two instances, the warmup left to the default",
            capacity: 2,
            warmup: None,
        },
        Replacing {
            case: "three instances, an explicit zero warmup",
            capacity: 3,
            warmup: Some("0"),
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        // One emulator per row: each row is its own account, so the ids of one row never
        // explain another's.
        let router = router_with(new_engine());
        let name = format!("asg-phases-running-{index}");
        create_group(&router, &name, row.capacity, None).await;
        let before = group_instance_ids(&router, &name).await;

        let preferences: Vec<(String, String)> = row
            .warmup
            .map(|warmup| vec![p("Preferences.InstanceWarmup", warmup)])
            .unwrap_or_default();
        let started = start_refresh(&router, &name, &preferences).await;

        // Read FIRST, before any step of the refresh can have come due: a refresh is
        // created running, never complete.
        let created = newest_refresh(&router, &name).await;
        assert!(
            RUNNING.contains(&created.status.as_str()),
            "case {:?}: a just-created refresh is still running, got {created:?}",
            row.case
        );
        assert_eq!(
            created.instance_refresh_id, started,
            "case {:?}: the group reports the refresh it was given, got {created:?}",
            row.case
        );
        assert!(
            created.end_time.is_empty(),
            "case {:?}: a running refresh has not ended, got {created:?}",
            row.case
        );
        second_refresh_is_refused(&router, &name, row.case).await;

        // Then it finishes — and what it reports then is what it did.
        let waited = until_status(&router, &name, row.case, &["Successful"]).await;
        let finished = waited.refresh;
        assert_eq!(
            finished.percentage_complete, "100",
            "case {:?}: a succeeded refresh is complete, got {finished:?}",
            row.case
        );
        assert_eq!(
            finished.instances_to_update, "0",
            "case {:?}: a succeeded refresh has nothing left to update, got {finished:?}",
            row.case
        );
        assert!(
            !finished.end_time.is_empty(),
            "case {:?}: a succeeded refresh reports when it ended, got {finished:?}",
            row.case
        );
        assert_eq!(
            finished.instance_refresh_id, started,
            "case {:?}: the refresh keeps the id it was started under, got {finished:?}",
            row.case
        );
        assert!(
            !waited.seen.contains(&"Baking".to_string()),
            "case {:?}: a refresh asked to wait nothing at the end never bakes, saw {:?}",
            row.case,
            waited.seen
        );

        // The group holds its capacity again, and holds NONE of the instances the refresh
        // replaced: the work the status reported was done.
        let after = group_instance_ids(&router, &name).await;
        assert_eq!(
            after.len(),
            usize::try_from(row.capacity).expect("a capacity is a count"),
            "case {:?}: the group holds its capacity after the refresh, got {after:?}",
            row.case
        );
        let kept: Vec<&String> = after.iter().filter(|id| before.contains(id)).collect();
        assert!(
            kept.is_empty(),
            "case {:?}: every instance the refresh replaced is gone, {kept:?} of {before:?} \
             are still held",
            row.case
        );
    }
}

/// Every row: while the refresh is still replacing instances a caller can see it, no
/// second refresh may start, and a cancellation answers that refresh's own id and drives
/// it to `Cancelled` — after which the group has nothing left to cancel.
#[tokio::test]
async fn a_refresh_still_replacing_instances_is_cancellable() {
    let rows = [
        Replacing {
            case: "one instance, the warmup and healthy percentage a caller states",
            capacity: 1,
            warmup: Some("120"),
        },
        Replacing {
            case: "two instances, a warmup of a minute",
            capacity: 2,
            warmup: Some("60"),
        },
        Replacing {
            case: "three instances, the warmup a caller states",
            capacity: 3,
            warmup: Some("120"),
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let router = router_with(new_engine());
        let name = format!("asg-phases-cancel-{index}");
        create_group(&router, &name, row.capacity, None).await;

        let mut preferences = vec![p("Preferences.MinHealthyPercentage", "90")];
        if let Some(warmup) = row.warmup {
            preferences.push(p("Preferences.InstanceWarmup", warmup));
        }
        let started = start_refresh(&router, &name, &preferences).await;

        // The refresh is observable while it replaces instances: it reaches `InProgress`
        // and a caller watching it sees it there.
        let waited = until_status(&router, &name, row.case, &["InProgress"]).await;
        let running = waited.refresh;
        assert!(
            running.end_time.is_empty(),
            "case {:?}: a refresh in progress has not ended, got {running:?}",
            row.case
        );
        second_refresh_is_refused(&router, &name, row.case).await;

        // The cancellation acts on THAT refresh, by its own id.
        let (status, body) = query(
            &router,
            "CancelInstanceRefresh",
            &[("AutoScalingGroupName", name.as_str())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: CancelInstanceRefresh expected 200, got {status}: {body}",
            row.case
        );
        assert_eq!(
            tag_value(&body, "InstanceRefreshId"),
            started,
            "case {:?}: the cancellation answers the refresh it acted on, got {body}",
            row.case
        );

        let cancelled = until_status(&router, &name, row.case, &["Cancelled"]).await.refresh;
        assert!(
            !cancelled.end_time.is_empty(),
            "case {:?}: a cancelled refresh reports when it ended, got {cancelled:?}",
            row.case
        );
        assert_eq!(
            cancelled.instance_refresh_id, started,
            "case {:?}: the cancelled refresh keeps its id, got {cancelled:?}",
            row.case
        );

        // Nothing is active any more, so there is nothing left to cancel.
        let (status, body) = query(
            &router,
            "CancelInstanceRefresh",
            &[("AutoScalingGroupName", name.as_str())],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {:?}: a second cancellation must be refused, got {status}: {body}",
            row.case
        );
        assert_eq!(
            tag_value(&body, "Code"),
            "ActiveInstanceRefreshNotFound",
            "case {:?}: expected the ActiveInstanceRefreshNotFound code, got {body}",
            row.case
        );
        let expected = no_active_refresh_message(&name);
        assert_eq!(
            xml_unescape(&tag_value(&body, "Message")),
            expected,
            "case {:?}: expected the message {expected:?}, got {body}",
            row.case
        );
    }
}

/// One row of the bake-time table.
struct Baked {
    case: &'static str,
    /// How many instances the group holds when the refresh starts.
    capacity: i32,
    /// The `BakeTime` the request states, or `None` to state none at all.
    bake_time: Option<&'static str>,
}

/// Every row: a refresh asked to wait at the end of the replacement is reported `Baking`
/// — every instance updated, not yet ended — until that wait has passed, and only then
/// `Successful`. A refresh asked to wait nothing is complete as soon as it has replaced
/// what it set out to.
#[tokio::test]
async fn a_refresh_asked_to_wait_at_the_end_bakes_before_it_succeeds() {
    let rows = [
        Baked {
            case: "no bake time stated at all",
            capacity: 1,
            bake_time: None,
        },
        Baked {
            case: "a bake time of zero, the published minimum",
            capacity: 1,
            bake_time: Some("0"),
        },
        Baked {
            case: "the seconds a caller states",
            capacity: 1,
            bake_time: Some(SHORT_BAKE_SECONDS),
        },
        Baked {
            case: "a group holding no instance still waits the bake time it was given",
            capacity: 0,
            bake_time: Some(SHORT_BAKE_SECONDS),
        },
        Baked {
            case: "two instances and the seconds a caller states",
            capacity: 2,
            bake_time: Some(SHORT_BAKE_SECONDS),
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let router = router_with(new_engine());
        let name = format!("asg-phases-bake-{index}");
        create_group(&router, &name, row.capacity, None).await;

        let preferences: Vec<(String, String)> = row
            .bake_time
            .map(|seconds| vec![p("Preferences.BakeTime", seconds)])
            .unwrap_or_default();
        let started = start_refresh(&router, &name, &preferences).await;

        // A bake time of nothing is nothing to wait: the refresh is never reported baking
        // and succeeds as soon as it has replaced the group's instances.
        let bakes = row.bake_time.is_some_and(|seconds| {
            seconds
                .parse::<i64>()
                .expect("a bake time a case states is a number of seconds")
                > BAKE_TIME_MINIMUM
        });
        if bakes {
            let baking = until_status(&router, &name, row.case, &["Baking"]).await.refresh;
            assert_eq!(
                baking.instance_refresh_id, started,
                "case {:?}: the baking refresh is the one that was started, got {baking:?}",
                row.case
            );
            assert_eq!(
                baking.bake_time,
                row.bake_time.unwrap_or_default(),
                "case {:?}: the refresh reports the bake time it runs under, got {baking:?}",
                row.case
            );
            assert_eq!(
                baking.instances_to_update, "0",
                "case {:?}: a baking refresh has finished updating instances, got {baking:?}",
                row.case
            );
            assert_eq!(
                baking.percentage_complete, "100",
                "case {:?}: a baking refresh has updated every instance, got {baking:?}",
                row.case
            );
            assert!(
                baking.end_time.is_empty(),
                "case {:?}: a baking refresh has not ended, got {baking:?}",
                row.case
            );
            // It still holds its group while it bakes.
            second_refresh_is_refused(&router, &name, row.case).await;
        }

        let waited = until_status(&router, &name, row.case, &["Successful"]).await;
        let finished = waited.refresh;
        assert!(
            !finished.end_time.is_empty(),
            "case {:?}: a succeeded refresh reports when it ended, got {finished:?}",
            row.case
        );
        assert_eq!(
            finished.percentage_complete, "100",
            "case {:?}: a succeeded refresh is complete, got {finished:?}",
            row.case
        );
        assert_eq!(
            waited.seen.contains(&"Baking".to_string()),
            bakes,
            "case {:?}: expected baking to be reported: {bakes}, saw {:?}",
            row.case,
            waited.seen
        );
    }
}

/// The longest bake time the model admits keeps the refresh non-terminal: it is the
/// group's active refresh while it bakes, and a cancellation acts on it.
#[tokio::test]
async fn a_refresh_baking_for_the_published_maximum_is_the_groups_active_refresh() {
    let case = "a bake time of the published maximum";
    let router = router_with(new_engine());
    let name = "asg-phases-bake-maximum";
    create_group(&router, name, 1, None).await;

    let started = start_refresh(
        &router,
        name,
        &[p("Preferences.BakeTime", BAKE_TIME_MAXIMUM.to_string())],
    )
    .await;
    let baking = until_status(&router, name, case, &["Baking"]).await.refresh;
    assert!(
        baking.end_time.is_empty(),
        "case {case:?}: a baking refresh has not ended, got {baking:?}"
    );
    assert_eq!(
        baking.bake_time,
        BAKE_TIME_MAXIMUM.to_string(),
        "case {case:?}: the refresh reports the bake time it runs under, got {baking:?}"
    );
    second_refresh_is_refused(&router, name, case).await;

    let (status, body) = query(
        &router,
        "CancelInstanceRefresh",
        &[("AutoScalingGroupName", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CancelInstanceRefresh expected 200, got {status}: {body}"
    );
    assert_eq!(
        tag_value(&body, "InstanceRefreshId"),
        started,
        "case {case:?}: the cancellation answers the baking refresh's own id, got {body}"
    );
    let cancelled = until_status(&router, name, case, &["Cancelled"]).await.refresh;
    assert!(
        !cancelled.end_time.is_empty(),
        "case {case:?}: a cancelled refresh reports when it ended, got {cancelled:?}"
    );
}

/// The launch template a desired configuration moves a group to, so a refresh carrying
/// one has a configuration to be rolled back FROM.
const DESIRED_TEMPLATE: &str = "lt-refresh-next";

/// The launch template version a group that can be rolled back pins: a numbered version
/// names one configuration, which is what a rollback goes back to.
const PINNED_VERSION: &str = "1";

/// A rollback acts on a refresh that is baking, which is what the sentence it refuses a
/// group with nothing to roll back with names: "No in progress or baking Instance Refresh
/// found …".
#[tokio::test]
async fn a_baking_refresh_can_still_be_rolled_back() {
    let case = "a refresh baking after replacing the group's one instance";
    let router = router_with(new_engine());
    let name = "asg-phases-bake-rollback";
    create_group(&router, name, 1, Some(PINNED_VERSION)).await;

    let started = start_refresh(
        &router,
        name,
        &[
            p("Preferences.BakeTime", BAKE_TIME_MAXIMUM.to_string()),
            p(
                "DesiredConfiguration.LaunchTemplate.LaunchTemplateName",
                DESIRED_TEMPLATE,
            ),
            p("DesiredConfiguration.LaunchTemplate.Version", PINNED_VERSION),
        ],
    )
    .await;
    until_status(&router, name, case, &["Baking"]).await;

    let (status, body) = query(
        &router,
        "RollbackInstanceRefresh",
        &[("AutoScalingGroupName", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: RollbackInstanceRefresh expected 200, got {status}: {body}"
    );
    assert_eq!(
        tag_value(&body, "InstanceRefreshId"),
        started,
        "case {case:?}: the rollback answers the baking refresh's own id, got {body}"
    );

    // The rollback replaces the refresh's own replacement back, and the refresh reports
    // none of the progress it had made.
    let rolled_back = until_status(&router, name, case, &["RollbackSuccessful"])
        .await
        .refresh;
    assert_eq!(
        rolled_back.percentage_complete, "0",
        "case {case:?}: a rolled-back refresh has none of its progress left, got {rolled_back:?}"
    );
    assert!(
        !rolled_back.end_time.is_empty(),
        "case {case:?}: a rolled-back refresh reports when it ended, got {rolled_back:?}"
    );

    // The group launches from the template it was using before the refresh: the desired
    // configuration never became its own, because the refresh never succeeded.
    let (status, body) = query(
        &router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the read-back expected 200, got {status}: {body}"
    );
    assert_eq!(
        tag_value(&body, "LaunchTemplateName"),
        LAUNCH_TEMPLATE,
        "case {case:?}: the group keeps the launch source it had, got {body}"
    );
}

/// A bake time outside the range the model publishes for it is refused, and the group
/// gets no refresh from it.
#[tokio::test]
async fn a_bake_time_outside_the_published_range_is_refused() {
    let router = router_with(new_engine());
    let name = "asg-phases-bake-bounds";
    create_group(&router, name, 0, None).await;

    for (case, seconds, bound) in [
        (
            "one second below the published minimum",
            BAKE_TIME_MINIMUM - 1,
            BAKE_TIME_MINIMUM,
        ),
        (
            "one second past the published maximum",
            BAKE_TIME_MAXIMUM + 1,
            BAKE_TIME_MAXIMUM,
        ),
    ] {
        let params = vec![
            p("AutoScalingGroupName", name),
            p("Preferences.BakeTime", seconds.to_string()),
        ];
        let (status, body) = query(&router, "StartInstanceRefresh", &borrowed(&params)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status}: {body}"
        );
        assert_eq!(
            tag_value(&body, "Code"),
            "ValidationError",
            "case {case:?}: expected the ValidationError code, got {body}"
        );
        let message = xml_unescape(&tag_value(&body, "Message"));
        assert!(
            message.contains(&bound.to_string()) && message.contains("constraint"),
            "case {case:?}: expected the refusal to name the bound {bound}, got {message:?}"
        );
    }

    // Nothing was started, so the group still has no refresh at all.
    let (status, body) = query(
        &router,
        "DescribeInstanceRefreshes",
        &[("AutoScalingGroupName", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeInstanceRefreshes expected 200, got {status}: {body}"
    );
    assert!(
        tag_values(&body, "InstanceRefreshId").is_empty(),
        "a refused bake time starts no refresh, got {body}"
    );
}
