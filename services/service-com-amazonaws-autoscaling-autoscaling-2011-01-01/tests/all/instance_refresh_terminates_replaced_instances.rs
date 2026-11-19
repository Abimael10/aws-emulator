// SPDX-License-Identifier: CC0-1.0

//! A refresh reported `Successful` has FINISHED terminating the instances it replaced.
//!
//! RULE under test: an instance refresh's default strategy "`Rolling` - Terminates
//! instances and launches replacements in batches"
//! (`smithy_query(query="shape",shape="RefreshStrategy")`) and "`Successful` - An instance
//! refresh completed successfully"
//! (`smithy_query(query="doc",shape="InstanceRefresh",member="Status")`), so the work the
//! refresh reports complete is work the account's own state already holds: every instance
//! the group held before the refresh is `terminated` in Amazon EC2 by the time
//! `DescribeInstanceRefreshes` reports the refresh `Successful`, whatever instant the
//! client happens to read it at — and the capacity the group then serves is the
//! replacements alone.
//!
//! The rows span what that rule generalises over: how many instances the group holds (one,
//! two, three), where the configuration the refresh moves them to comes from (the group's
//! own updated launch template, or a `DesiredConfiguration` the request carries), the
//! warmup the refresh runs under, and — the axis this is really about — WHEN the client
//! looks: a client that polls as fast as it can, and one whose next poll lands only once
//! the clock is already past every step the refresh owed, so the whole refresh is
//! materialised in one pass. Neither may see a `Successful` refresh whose replaced
//! instances are still running.
//!
//! Both services are mounted on ONE engine and driven over their own wire protocols —
//! awsQuery for Auto Scaling, ec2Query for Amazon EC2 — so the instances these assertions
//! read are the account's own.

use std::time::Duration;

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;
use service_com_amazonaws_ec2_amazonec2::state as ec2;

use crate::support::{borrowed, ec2_query, emulator, p, query, tag_value, tag_values};

/// The AMI every launch template in this suite carries.
const IMAGE_ID: &str = "ami-00000000000000041";

/// The instance type the group's instances run BEFORE the refresh, and the one the version
/// the refresh moves them to runs. Two members of Amazon EC2's own instance-type catalog,
/// so the replacement's type is a fact EC2 reports rather than a string this suite invents.
const OLD_INSTANCE_TYPE: &str = "t3.micro";
const NEW_INSTANCE_TYPE: &str = "t3.small";

/// The zone the groups in this suite place their instances in.
const ZONE: &str = "us-west-2a";

/// The lifecycle state an instance that is serving carries.
const IN_SERVICE: &str = "InService";

/// A BACKSTOP for every wait below, not a schedule: nothing in the emulator sleeps for a
/// due instant, so each wait looks again until the condition it needs holds. Generous on
/// purpose — these rows run beside every other suite — and derived from the service's own
/// spans, so this and the implementation cannot drift apart.
fn settle_deadline() -> Duration {
    (autoscaling::state::INSTANCE_REFRESH_REPLACEMENT
        .max(autoscaling::state::LIFECYCLE_TRANSITION)
        .max(ec2::TRANSITION_DELAY))
        * 60
}

/// How long the span of ONE refresh step is, as the service itself states it.
fn refresh_step() -> Duration {
    autoscaling::state::INSTANCE_REFRESH_REPLACEMENT.max(autoscaling::state::LIFECYCLE_TRANSITION)
}

/// How often a client that watches a refresh as closely as it can looks again.
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// When the client's next poll lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Watched {
    /// As closely as a client can: every [`LOOK_AGAIN`], so each step of the refresh is
    /// materialised at about the instant it came due.
    Closely,
    /// Only once the clock is already past every step the refresh owed, so the first poll
    /// that sees anything sees the WHOLE refresh materialised in one pass.
    Late,
}

impl Watched {
    /// How long this client waits before looking again, for a refresh of `capacity`
    /// instances.
    fn look_again(self, capacity: i32) -> Duration {
        match self {
            Self::Closely => LOOK_AGAIN,
            // One span per instance the refresh replaces, one for the refresh's own
            // completion and one more so the poll lands past it however slowly the host
            // runs.
            Self::Late => refresh_step() * (u32::try_from(capacity).unwrap_or(1) + 2),
        }
    }
}

/// Where the configuration the refresh moves the group's instances to comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MovedBy {
    /// `UpdateAutoScalingGroup` puts the new launch-template version on the group, and the
    /// refresh names no desired configuration of its own.
    GroupUpdate,
    /// The refresh carries `DesiredConfiguration.LaunchTemplate` naming that version.
    DesiredConfiguration,
}

/// One row of the table.
struct Row {
    case: &'static str,
    /// How many instances the group holds when the refresh starts.
    capacity: i32,
    /// Where the new launch-template version reaches the refresh from.
    moved_by: MovedBy,
    /// `Preferences.InstanceWarmup`, the wait the refresh runs under after a replacement
    /// enters service.
    warmup: &'static str,
    /// When the client's poll lands.
    watched: Watched,
}

const ROWS: &[Row] = &[
    Row {
        case: "one instance, the group updated to the new version, polled closely",
        capacity: 1,
        moved_by: MovedBy::GroupUpdate,
        warmup: "0",
        watched: Watched::Closely,
    },
    Row {
        case: "one instance, the group updated to the new version, polled late",
        capacity: 1,
        moved_by: MovedBy::GroupUpdate,
        warmup: "0",
        watched: Watched::Late,
    },
    Row {
        case: "two instances, a desired configuration, polled late",
        capacity: 2,
        moved_by: MovedBy::DesiredConfiguration,
        warmup: "0",
        watched: Watched::Late,
    },
    Row {
        case: "three instances under a warmup, the group updated, polled closely",
        capacity: 3,
        moved_by: MovedBy::GroupUpdate,
        warmup: "1",
        watched: Watched::Closely,
    },
];

/// Create one Amazon EC2 launch template over that service's own wire, running
/// [`OLD_INSTANCE_TYPE`].
async fn create_launch_template(router: &MultiServiceRouter, name: &str) {
    let (status, body) = ec2_query(
        router,
        "CreateLaunchTemplate",
        &[
            p("LaunchTemplateName", name),
            p("LaunchTemplateData.ImageId", IMAGE_ID),
            p("LaunchTemplateData.InstanceType", OLD_INSTANCE_TYPE),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {name:?}: CreateLaunchTemplate expected 200, got {status}: {body}"
    );
}

/// Add a version running [`NEW_INSTANCE_TYPE`] to the template `name` and answer the
/// version number Amazon EC2 minted for it.
async fn add_launch_template_version(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = ec2_query(
        router,
        "CreateLaunchTemplateVersion",
        &[
            p("LaunchTemplateName", name),
            p("SourceVersion", "1"),
            p("LaunchTemplateData.ImageId", IMAGE_ID),
            p("LaunchTemplateData.InstanceType", NEW_INSTANCE_TYPE),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {name:?}: CreateLaunchTemplateVersion expected 200, got {status}: {body}"
    );
    let version = tag_value(&body, "versionNumber");
    assert!(
        !version.is_empty(),
        "case {name:?}: CreateLaunchTemplateVersion must report the version it minted, got {body}"
    );
    version
}

/// The ids of the instances one group reports `InService`, in the order it reports them.
async fn in_service_ids(router: &MultiServiceRouter, group: &str) -> Vec<String> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeAutoScalingGroups for {group:?} expected 200, got {status}: {body}"
    );
    let ids = tag_values(&body, "InstanceId");
    let states = tag_values(&body, "LifecycleState");
    ids.into_iter()
        .zip(states)
        .filter(|(_, state)| state == IN_SERVICE)
        .map(|(id, _)| id)
        .collect()
}

/// Look again until the group serves `wanted` instances, or the deadline passes; answers
/// what it last saw either way.
async fn settled_in_service_ids(
    router: &MultiServiceRouter,
    group: &str,
    wanted: usize,
) -> Vec<String> {
    let deadline = tokio::time::Instant::now() + settle_deadline();
    loop {
        let serving = in_service_ids(router, group).await;
        if serving.len() == wanted || tokio::time::Instant::now() >= deadline {
            return serving;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The `Status` of the newest refresh of one group.
async fn refresh_status(router: &MultiServiceRouter, group: &str) -> String {
    let (status, body) = query(
        router,
        "DescribeInstanceRefreshes",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeInstanceRefreshes for {group:?} expected 200, got {status}: {body}"
    );
    tag_value(&body, "Status")
}

/// The statuses a refresh never leaves, as the service's own catalog spells them.
fn terminal_statuses() -> [&'static str; 5] {
    [
        autoscaling::state::REFRESH_SUCCESSFUL,
        autoscaling::state::REFRESH_FAILED,
        autoscaling::state::REFRESH_CANCELLED,
        autoscaling::state::REFRESH_ROLLBACK_SUCCESSFUL,
        autoscaling::state::REFRESH_ROLLBACK_FAILED,
    ]
}

/// Look again, `look_again` apart, until the group's newest refresh reports a status it
/// never leaves; answers that status.
///
/// Waits for the STATUS rather than for a span, so the only thing `look_again` decides is
/// HOW LATE the poll that observes the refresh lands — which is the axis the rows span.
async fn watch_until_terminal(
    router: &MultiServiceRouter,
    group: &str,
    look_again: Duration,
    case: &str,
) -> String {
    let deadline = tokio::time::Instant::now() + settle_deadline();
    loop {
        let status = refresh_status(router, group).await;
        if terminal_statuses().contains(&status.as_str()) {
            return status;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: expected the refresh of {group:?} to reach a terminal status, \
             last saw {status:?}"
        );
        tokio::time::sleep(look_again).await;
    }
}

/// What Amazon EC2 reports for one instance, as `(state name, instance type)`. Both empty
/// when EC2 resolves the id to nothing at all.
async fn described_instance(router: &MultiServiceRouter, instance_id: &str) -> (String, String) {
    let (status, body) =
        ec2_query(router, "DescribeInstances", &[p("InstanceId.1", instance_id)]).await;
    if status != 200 {
        return (String::new(), String::new());
    }
    (
        tag_value(&tag_value(&body, "instanceState"), "name"),
        tag_value(&body, "instanceType"),
    )
}

/// Move every step the group's refresh still owes `behind` into the PAST, and answer how
/// many it moved.
///
/// This is the state a host that did not get round to the clock for a while leaves behind:
/// the steps were owed at instants that have gone by, so the next pass retires all of them
/// at once instead of one per span. Nothing else is touched — the steps themselves, and
/// everything they do to the group and to Amazon EC2, are the implementation's own.
fn fall_behind(engine: &engine::InMemoryEngine, group: &str, behind: Duration) -> usize {
    let state = autoscaling::state::service_state(engine)
        .expect("the Auto Scaling store is mounted on this engine");
    let mut store = state.lock();
    let rewound = std::time::SystemTime::now()
        .checked_sub(behind)
        .expect("an instant one refresh's worth of spans ago");
    let mut moved = 0;
    for refresh in &mut store.instance_refreshes {
        if refresh.auto_scaling_group_name == group && refresh.next_step_due.is_some() {
            refresh.next_step_due = Some(rewound);
            moved += 1;
        }
    }
    moved
}

/// The same requirement where the clock FELL BEHIND: a pass that retires the whole refresh
/// at once — every step of it owed at an instant that has already gone by — still has to
/// have finished terminating the instances it replaced before it reports itself
/// `Successful`.
///
/// Where the table above spans how late the CLIENT looks, this spans how late the SERVICE
/// itself gets round to the steps it owes, which is the other half of the same rule: the
/// span Amazon EC2's own move to `terminated` runs out is the span since the termination
/// was owed, not since somebody finally looked.
#[tokio::test]
async fn a_refresh_whose_steps_are_all_retired_in_one_pass_terminates_what_it_replaced() {
    let capacity = 2;
    let (router, engine) = emulator();
    let template = "lt-refresh-terminates-late-clock";
    create_launch_template(&router, template).await;

    let group = "asg-refresh-terminates-late-clock";
    let (status, body) = query(
        &router,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", group),
            ("LaunchTemplate.LaunchTemplateName", template),
            ("LaunchTemplate.Version", "1"),
            ("MinSize", "2"),
            ("MaxSize", "3"),
            ("DesiredCapacity", "2"),
            ("AvailabilityZones.member.1", ZONE),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateAutoScalingGroup expected 200, got {status}: {body}"
    );
    let wanted = usize::try_from(capacity).expect("a capacity is a count");
    let before = settled_in_service_ids(&router, group, wanted).await;
    assert_eq!(
        before.len(),
        wanted,
        "the group must serve the capacity it was created with, got {before:?}"
    );

    let new_version = add_launch_template_version(&router, template).await;
    let update = vec![
        p("AutoScalingGroupName", group),
        p("LaunchTemplate.LaunchTemplateName", template),
        p("LaunchTemplate.Version", new_version),
    ];
    let (status, body) = query(&router, "UpdateAutoScalingGroup", &borrowed(&update)).await;
    assert_eq!(
        status, 200,
        "UpdateAutoScalingGroup expected 200, got {status}: {body}"
    );

    let params = vec![
        p("AutoScalingGroupName", group),
        p("Preferences.MinHealthyPercentage", "100"),
        p("Preferences.MaxHealthyPercentage", "200"),
        p("Preferences.InstanceWarmup", "0"),
    ];
    let (status, body) = query(&router, "StartInstanceRefresh", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "StartInstanceRefresh expected 200, got {status}: {body}"
    );

    // Every step of the refresh was owed before this instant, so ONE pass retires them all.
    let behind = refresh_step() * (u32::try_from(capacity).unwrap_or(1) + 2);
    let moved = fall_behind(&engine, group, behind);
    assert_eq!(
        moved, 1,
        "the group must have exactly one refresh still owing steps, got {moved}"
    );

    // The read that drives that pass is the one that reports the refresh complete…
    let status = refresh_status(&router, group).await;
    assert_eq!(
        status,
        autoscaling::state::REFRESH_SUCCESSFUL,
        "a refresh every one of whose steps was owed before now is complete, got {status:?}"
    );
    // …and by then the instances it replaced are terminated, read with no further waiting.
    for instance_id in &before {
        let (state, _) = described_instance(&router, instance_id).await;
        assert_eq!(
            state, "terminated",
            "{instance_id} was replaced by a refresh that reports itself {status:?}, so \
             Amazon EC2 must report it terminated, got {state:?}"
        );
    }

    let after = settled_in_service_ids(&router, group, wanted).await;
    assert_eq!(
        after.len(),
        wanted,
        "the group must serve its capacity after the refresh, got {after:?}"
    );
    assert!(
        after.iter().all(|id| !before.contains(id)),
        "every instance of {before:?} had to be replaced, got {after:?}"
    );
}

#[tokio::test]
async fn a_successful_refresh_has_terminated_every_instance_it_replaced() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        // One emulator per row: each row is its own account, so the ids of one row never
        // explain another's.
        let (router, _engine) = emulator();
        let template = format!("lt-refresh-terminates-{index}");
        create_launch_template(&router, &template).await;

        let group = format!("asg-refresh-terminates-{index}");
        let capacity = row.capacity.to_string();
        // `MaxHealthyPercentage` 200 lets the refresh SURGE a replacement before taking the
        // instance it replaces out, so the group's maximum leaves room for one.
        let max_size = (row.capacity + 1).to_string();
        let (status, body) = query(
            &router,
            "CreateAutoScalingGroup",
            &[
                ("AutoScalingGroupName", group.as_str()),
                ("LaunchTemplate.LaunchTemplateName", template.as_str()),
                ("LaunchTemplate.Version", "1"),
                ("MinSize", capacity.as_str()),
                ("MaxSize", max_size.as_str()),
                ("DesiredCapacity", capacity.as_str()),
                ("AvailabilityZones.member.1", ZONE),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateAutoScalingGroup expected 200, got {status}: {body}"
        );

        let wanted = usize::try_from(row.capacity).expect("a capacity is a count");
        let before = settled_in_service_ids(&router, &group, wanted).await;
        assert_eq!(
            before.len(),
            wanted,
            "case {case:?}: the group must serve the capacity it was created with, got {before:?}"
        );
        for instance_id in &before {
            let (state, instance_type) = described_instance(&router, instance_id).await;
            assert_eq!(
                (state.as_str(), instance_type.as_str()),
                ("running", OLD_INSTANCE_TYPE),
                "case {case:?}: {instance_id} must be running the group's first version, \
                 got {state:?} / {instance_type:?}"
            );
        }

        // The version the refresh moves the group's instances onto, minted only once those
        // instances exist.
        let new_version = add_launch_template_version(&router, &template).await;
        let mut params = vec![
            p("AutoScalingGroupName", group.as_str()),
            p("Preferences.MinHealthyPercentage", "100"),
            p("Preferences.MaxHealthyPercentage", "200"),
            p("Preferences.InstanceWarmup", row.warmup),
        ];
        match row.moved_by {
            MovedBy::GroupUpdate => {
                let update = vec![
                    p("AutoScalingGroupName", group.as_str()),
                    p("LaunchTemplate.LaunchTemplateName", template.as_str()),
                    p("LaunchTemplate.Version", new_version.clone()),
                ];
                let (status, body) =
                    query(&router, "UpdateAutoScalingGroup", &borrowed(&update)).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: UpdateAutoScalingGroup expected 200, got {status}: {body}"
                );
            }
            MovedBy::DesiredConfiguration => {
                params.push(p(
                    "DesiredConfiguration.LaunchTemplate.LaunchTemplateName",
                    template.as_str(),
                ));
                params.push(p(
                    "DesiredConfiguration.LaunchTemplate.Version",
                    new_version.clone(),
                ));
            }
        }

        let (status, body) = query(&router, "StartInstanceRefresh", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: StartInstanceRefresh expected 200, got {status}: {body}"
        );
        assert!(
            !tag_value(&body, "InstanceRefreshId").is_empty(),
            "case {case:?}: the answer must carry the refresh's id, got {body}"
        );

        let status = watch_until_terminal(
            &router,
            &group,
            row.watched.look_again(row.capacity),
            case,
        )
        .await;
        assert_eq!(
            status,
            autoscaling::state::REFRESH_SUCCESSFUL,
            "case {case:?}: expected the refresh to have succeeded, got {status:?}"
        );

        // THE REQUIREMENT: the instances the refresh replaced are terminated in the account's
        // own Amazon EC2 store the moment it reports itself complete — read with no further
        // waiting, because a refresh that has not finished terminating them is not yet
        // `Successful`.
        for instance_id in &before {
            let (state, _) = described_instance(&router, instance_id).await;
            assert_eq!(
                state, "terminated",
                "case {case:?}: {instance_id} was replaced by a refresh that reports itself \
                 {status:?}, so Amazon EC2 must report it terminated, got {state:?}"
            );
        }

        // And what the group serves is the replacements alone, each running the version the
        // refresh moved it to.
        let after = in_service_ids(&router, &group).await;
        assert_eq!(
            after.len(),
            wanted,
            "case {case:?}: the group must serve its capacity after the refresh, got {after:?}"
        );
        assert!(
            after.iter().all(|id| !before.contains(id)),
            "case {case:?}: every instance of {before:?} had to be replaced, got {after:?}"
        );
        for instance_id in &after {
            let (state, instance_type) = described_instance(&router, instance_id).await;
            assert_eq!(
                (state.as_str(), instance_type.as_str()),
                ("running", NEW_INSTANCE_TYPE),
                "case {case:?}: the replacement {instance_id} must run the version the refresh \
                 moved the group to, got {state:?} / {instance_type:?}"
            );
        }
    }
}
