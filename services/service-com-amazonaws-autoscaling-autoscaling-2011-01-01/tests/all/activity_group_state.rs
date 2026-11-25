// SPDX-License-Identifier: CC0-1.0

//! `Activity$AutoScalingGroupState` is reported only where the state of a group is what
//! is being reported.
//!
//! The rule under test: the OPTIONAL `AutoScalingGroupState` member of an `Activity` is
//! emitted only by the read that reports the state of the account's groups — the
//! `DescribeScalingActivities` call whose request asked to be told about deleted groups —
//! and there it is read off whether the account still holds the group the activity
//! belongs to (`InService` while it does, `Deleted` once it does not). Every other answer
//! that carries an activity leaves the member out.
//!
//! The tables below span what that rule generalises over: whether the request opts into
//! deleted groups and how it spells the opt-in, whether the group the activities belong
//! to is one the account still holds, how a read selects the activities (one group by
//! name, one activity by id, account-wide), and each operation that answers with an
//! activity of its own.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{borrowed, new_engine, p, query, router_with, tag_value, tag_values};

/// The launch template the groups in this suite launch their instances from.
const LAUNCH_TEMPLATE: &str = "lt-1";

/// The zone the groups in this suite place their instances in.
const ZONE: &str = "us-west-2a";

/// The two values the member may carry: "The state of the Auto Scaling group, which is
/// either InService or Deleted"
/// (`smithy_query(query="doc",shape="Activity",member="AutoScalingGroupState")`).
const IN_SERVICE: &str = "InService";
const DELETED: &str = "Deleted";

/// The `@length` bound the model puts on `AutoScalingGroupState`: min 1, max 32
/// (`smithy_query(query="shape",shape="AutoScalingGroupState")`).
const STATE_LENGTH: std::ops::RangeInclusive<usize> = 1..=32;

/// How long a wait for a lifecycle transition is given before the suite calls it a
/// failure. Generous: the service's own span is far shorter, and this only has to be long
/// enough that a loaded host still gets there.
const LIFECYCLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long the poll below leaves between two looks at the group.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Create one group holding `capacity` instances.
async fn create_group(router: &MultiServiceRouter, name: &str, capacity: i32) {
    let capacity = capacity.to_string();
    let (status, body) = query(
        router,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", name),
            ("LaunchTemplate.LaunchTemplateName", LAUNCH_TEMPLATE),
            ("MinSize", "0"),
            ("MaxSize", "5"),
            ("DesiredCapacity", capacity.as_str()),
            ("AvailabilityZones.member.1", ZONE),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateAutoScalingGroup for {name} expected 200, got {status}: {body}"
    );
}

/// Read the scaling activities with the criteria `params` state, of a call that must have
/// succeeded.
async fn reported(router: &MultiServiceRouter, case: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = query(router, "DescribeScalingActivities", params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeScalingActivities expected 200, got {status}: {body}"
    );
    body
}

/// The id of an instance of `group` the group reports in `lifecycle`, waited for rather
/// than timed: the condition is looked at again until it holds or a generous deadline
/// passes.
async fn instance_in_state(router: &MultiServiceRouter, group: &str, lifecycle: &str) -> String {
    let deadline = Instant::now() + LIFECYCLE_DEADLINE;
    loop {
        let (status, body) = query(
            router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", group)],
        )
        .await;
        assert_eq!(
            status, 200,
            "DescribeAutoScalingGroups for {group} expected 200, got {status}: {body}"
        );
        let ids = tag_values(&body, "InstanceId");
        let states = tag_values(&body, "LifecycleState");
        if let Some(index) = states.iter().position(|state| state == lifecycle) {
            return ids[index].clone();
        }
        assert!(
            Instant::now() < deadline,
            "an instance of {group} must reach {lifecycle}, got {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// What the activities one read answers with must say about the state of their groups:
/// how many report the group in service and how many report it deleted. Both zero is a
/// read that reports no state at all.
#[derive(Debug)]
struct States {
    in_service: usize,
    deleted: usize,
}

/// One row of the read table: what a failing assertion calls it, the criteria the request
/// states, how many activities it must report and what those report about their groups.
struct Read {
    case: &'static str,
    params: Vec<(String, String)>,
    activities: usize,
    states: States,
}

/// A read reports `AutoScalingGroupState` only when the request asked about deleted
/// groups, and then reports the state the account's own hold on the group makes it.
#[tokio::test]
async fn the_group_state_is_reported_only_to_a_read_that_asked_about_deleted_groups() {
    let router = router_with(new_engine());
    // One group the account keeps, holding two instances, so each of its activities is
    // checked and not just the first.
    create_group(&router, "asg-held", 2).await;
    // One group the account tears down: its launch activity and the activity of the
    // delete itself both outlive it.
    create_group(&router, "asg-gone", 1).await;
    let (status, body) = query(
        &router,
        "DeleteAutoScalingGroup",
        &[
            ("AutoScalingGroupName", "asg-gone"),
            ("ForceDelete", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "DeleteAutoScalingGroup expected 200, got {status}: {body}"
    );

    // One activity of the group the account holds, to select a read by its id.
    let held = reported(&router, "the held group", &[("AutoScalingGroupName", "asg-held")]).await;
    let held_ids = tag_values(&held, "ActivityId");
    assert_eq!(
        held_ids.len(),
        2,
        "the held group must have recorded one activity per instance, got {held_ids:?}"
    );
    let held_id = held_ids[0].clone();

    let rows = vec![
        Read {
            case: "a group the account holds, with no opt-in",
            params: vec![p("AutoScalingGroupName", "asg-held")],
            activities: 2,
            states: States {
                in_service: 0,
                deleted: 0,
            },
        },
        Read {
            case: "a group the account holds, with the opt-in spelled false",
            params: vec![
                p("AutoScalingGroupName", "asg-held"),
                p("IncludeDeletedGroups", "false"),
            ],
            activities: 2,
            states: States {
                in_service: 0,
                deleted: 0,
            },
        },
        Read {
            case: "a group the account holds, asking about deleted groups",
            params: vec![
                p("AutoScalingGroupName", "asg-held"),
                p("IncludeDeletedGroups", "true"),
            ],
            activities: 2,
            states: States {
                in_service: 2,
                deleted: 0,
            },
        },
        Read {
            case: "a group the account deleted, asking about deleted groups",
            params: vec![
                p("AutoScalingGroupName", "asg-gone"),
                p("IncludeDeletedGroups", "true"),
            ],
            activities: 2,
            states: States {
                in_service: 0,
                deleted: 2,
            },
        },
        Read {
            case: "a group the account deleted, with no opt-in",
            params: vec![p("AutoScalingGroupName", "asg-gone")],
            activities: 0,
            states: States {
                in_service: 0,
                deleted: 0,
            },
        },
        Read {
            case: "every group of the account, with no opt-in",
            params: vec![],
            activities: 2,
            states: States {
                in_service: 0,
                deleted: 0,
            },
        },
        Read {
            case: "every group of the account, asking about deleted groups",
            params: vec![p("IncludeDeletedGroups", "true")],
            activities: 4,
            states: States {
                in_service: 2,
                deleted: 2,
            },
        },
        Read {
            case: "one activity by id, with no opt-in",
            params: vec![p("ActivityIds.member.1", held_id.clone())],
            activities: 1,
            states: States {
                in_service: 0,
                deleted: 0,
            },
        },
        Read {
            case: "one activity by id, asking about deleted groups",
            params: vec![
                p("ActivityIds.member.1", held_id.clone()),
                p("IncludeDeletedGroups", "true"),
            ],
            activities: 1,
            states: States {
                in_service: 1,
                deleted: 0,
            },
        },
    ];

    for row in rows {
        let body = reported(&router, row.case, &borrowed(&row.params)).await;
        let ids = tag_values(&body, "ActivityId");
        assert_eq!(
            ids.len(),
            row.activities,
            "case {:?}: expected {} activities, got {ids:?} from {body}",
            row.case,
            row.activities
        );
        let states = tag_values(&body, "AutoScalingGroupState");
        assert_eq!(
            states.len(),
            row.states.in_service + row.states.deleted,
            "case {:?}: expected {:?}, got the states {states:?} in {body}",
            row.case,
            row.states
        );
        for state in &states {
            assert!(
                state == IN_SERVICE || state == DELETED,
                "case {:?}: a reported state is one of the two the model names, got {state:?}",
                row.case
            );
            assert!(
                STATE_LENGTH.contains(&state.chars().count()),
                "case {:?}: a reported state is within the modelled length, got {state:?}",
                row.case
            );
        }
        assert_eq!(
            states.iter().filter(|state| *state == IN_SERVICE).count(),
            row.states.in_service,
            "case {:?}: expected {:?}, got the states {states:?} in {body}",
            row.case,
            row.states
        );
        assert_eq!(
            states.iter().filter(|state| *state == DELETED).count(),
            row.states.deleted,
            "case {:?}: expected {:?}, got the states {states:?} in {body}",
            row.case,
            row.states
        );
    }
}

/// Every operation that answers with an activity of its own reports that activity without
/// an `AutoScalingGroupState`: its answer is about the step it just recorded, not about
/// the state of the account's groups.
#[tokio::test]
async fn an_operation_that_answers_with_an_activity_reports_no_group_state() {
    for (index, operation) in [
        "TerminateInstanceInAutoScalingGroup",
        "EnterStandby",
        "ExitStandby",
        "DetachInstances",
    ]
    .into_iter()
    .enumerate()
    {
        // One emulator per row: the group one row acts on is not the next row's.
        let router = router_with(new_engine());
        let group = format!("asg-answer-{index}");
        create_group(&router, &group, 1).await;
        let instance = instance_in_state(&router, &group, "InService").await;

        let params: Vec<(String, String)> = match operation {
            "TerminateInstanceInAutoScalingGroup" => vec![
                p("InstanceId", instance.clone()),
                p("ShouldDecrementDesiredCapacity", "true"),
            ],
            "EnterStandby" => vec![
                p("AutoScalingGroupName", group.clone()),
                p("InstanceIds.member.1", instance.clone()),
                p("ShouldDecrementDesiredCapacity", "true"),
            ],
            "ExitStandby" => {
                // An instance comes back from standby, so it has to be parked first.
                let (status, body) = query(
                    &router,
                    "EnterStandby",
                    &[
                        ("AutoScalingGroupName", group.as_str()),
                        ("InstanceIds.member.1", instance.as_str()),
                        ("ShouldDecrementDesiredCapacity", "true"),
                    ],
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {operation:?}: EnterStandby expected 200, got {status}: {body}"
                );
                instance_in_state(&router, &group, "Standby").await;
                vec![
                    p("AutoScalingGroupName", group.clone()),
                    p("InstanceIds.member.1", instance.clone()),
                ]
            }
            "DetachInstances" => vec![
                p("AutoScalingGroupName", group.clone()),
                p("InstanceIds.member.1", instance.clone()),
                p("ShouldDecrementDesiredCapacity", "true"),
            ],
            other => unreachable!("case {other:?}: the table names only operations it builds"),
        };

        let (status, body) = query(&router, operation, &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {operation:?}: expected 200, got {status}: {body}"
        );
        assert!(
            !tag_value(&body, "ActivityId").is_empty(),
            "case {operation:?}: the answer must report the activity it recorded, got {body}"
        );
        let state = tag_value(&body, "AutoScalingGroupState");
        assert!(
            state.is_empty(),
            "case {operation:?}: expected no AutoScalingGroupState, got {state:?} in {body}"
        );
    }
}
