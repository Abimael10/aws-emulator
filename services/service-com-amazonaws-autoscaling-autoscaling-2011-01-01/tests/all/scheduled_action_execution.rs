// SPDX-License-Identifier: CC0-1.0

//! What a stored scheduled scaling action DOES when its occurrence comes due, over the
//! awsQuery wire.
//!
//! The rules under test:
//!
//! 1. A scheduled action is a capacity change the CLOCK owes: once the instant it is timed at
//!    has passed, the sizes it states are the group's — a bound it does not state keeping the
//!    one the group already has — and `DescribeAutoScalingGroups` reports them. Whether the
//!    action is timed by `StartTime`, by the legacy `Time`, or carries a `Recurrence` beside
//!    the instant it starts at ("if you specify Recurrence and StartTime, Amazon EC2 Auto
//!    Scaling performs the action at this time, and then performs the action based on the
//!    specified recurrence", `PutScheduledUpdateGroupActionType$StartTime`).
//! 2. Performing a due occurrence is the group's own `ScheduledActions` process, so a group
//!    that has suspended it SKIPS the occurrence: its sizes do not move, the schedule goes on
//!    to the occurrence after it, and resuming the process does not bring the missed one
//!    back.
//! 3. An action that has run its last occurrence, and one whose `EndTime` has passed, is no
//!    longer scheduled and drops out of `DescribeScheduledActions`; one whose occurrence is
//!    still ahead stays, and a recurring one reports the occurrence that follows the one it
//!    just ran.
//! 4. `StartTime` and `EndTime` are "the date and time […] in UTC. For example,
//!    \"2021-06-01T00:00:00Z\"" (`PutScheduledUpdateGroupActionType$StartTime`, `$EndTime`):
//!    a sub-second fraction a request states is truncated to whole seconds on write — dropped,
//!    never rounded up — so a read reports the instant the schedule holds.
//! 5. What the schedule cannot take is refused before the sizes are weighed: an instant the
//!    clock has already passed ("if you try to schedule your action in the past, Amazon EC2
//!    Auto Scaling returns an error message",
//!    `smithy_query(query="operation",operation="PutScheduledUpdateGroupAction")`) and a
//!    `Recurrence` that is not the documented crontab expression are both refused with HTTP
//!    400 `ValidationError`, and nothing is stored.
//!
//! Each table spans what its rule generalises over — which sizes the action sets, how it is
//! timed, which fraction an instant carries, how far in the past an instant is, which part of
//! a recurrence is unreadable — rather than one recorded row. Every wait is for the
//! CONDITION, under a generous deadline that only turns a condition which never holds into a
//! failed assertion.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    borrowed, list_members, max_below_min_message, new_engine, p, query, router_with, tag_value,
    xml_unescape,
};

/// The launch source every group here is created from — a launch TEMPLATE, because no account
/// can create a launch configuration.
const LAUNCH_SOURCE: (&str, &str) = ("LaunchTemplate.LaunchTemplateName", "lt-1");

/// Where every group here places the instances a due occurrence has it launch.
const ZONE: (&str, &str) = ("AvailabilityZones.member.1", "us-west-2a");

/// The maximum size every group here is created with, which the rows' sizes sit inside.
const GROUP_MAX_SIZE: i32 = 3;

/// How far ahead of the call a row times an action it wants to see FIRE.
///
/// Generous rather than tight: the instant has to be ahead of the clock when the service
/// reads the request, so a loaded machine cannot turn an accepted schedule into a refused
/// one, and it is not what any case here measures.
const FIRES_IN: Duration = Duration::from_secs(3);

/// How far ahead a row times an action it wants to stay PENDING for the whole test.
const STAYS_PENDING_IN: Duration = Duration::from_secs(60 * 60);

/// How long a case that waits for an occurrence keeps looking before it calls the wait a
/// failure: far above [`FIRES_IN`] and the span one capacity change takes, because what is
/// waited for is the condition.
const DEADLINE: Duration = Duration::from_secs(90);

/// How long such a case waits between looks.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The whole message a put whose start time the clock has already passed is refused with:
/// entirely fixed text, with no part of the request in it.
const START_TIME_IN_PAST_MESSAGE: &str = "Given start time is in the past";

/// The whole message a put whose `Recurrence` is not the documented crontab expression is
/// refused with, quoting the value that was rejected.
fn invalid_recurrence_message(recurrence: &str) -> String {
    format!("Given recurrence string: {recurrence} is invalid")
}

/// The whole message every writer of a group's three sizes states for a capacity outside the
/// bounds it would leave the group with — the refusal a bad recurrence must NOT be answered
/// with.
fn desired_capacity_message(desired_capacity: i32, min_size: i32, max_size: i32) -> String {
    format!(
        "Desired capacity:{desired_capacity} must be between the specified min \
         size:{min_size} and max size:{max_size}"
    )
}

/// `at` as the RFC 3339 instant a request states a timestamp in, to the second.
fn rfc3339(at: SystemTime) -> String {
    let seconds = at.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    ::aws_smithy_types::DateTime::from_secs(i64::try_from(seconds).unwrap_or_default())
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .expect("the instant renders")
}

/// The instant `ahead` from now, as a request states it.
fn instant_ahead(ahead: Duration) -> String {
    rfc3339(SystemTime::now() + ahead)
}

/// The instant `behind` before now, as a request states it.
fn instant_behind(behind: Duration) -> String {
    rfc3339(SystemTime::now() - behind)
}

/// Create one group with the bounds every row here starts from: nothing running, and room to
/// grow to [`GROUP_MAX_SIZE`].
async fn create_group(router: &MultiServiceRouter, name: &str, max_size: i32) {
    let params = vec![
        p("AutoScalingGroupName", name),
        p(LAUNCH_SOURCE.0, LAUNCH_SOURCE.1),
        p("MinSize", "0"),
        p("MaxSize", max_size.to_string()),
        p("DesiredCapacity", "0"),
        p(ZONE.0, ZONE.1),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "creating {name:?}: expected 200, got {status} with {body}"
    );
}

/// Post one `PutScheduledUpdateGroupAction`, and answer its status, wire error code and
/// message.
async fn put_action(
    router: &MultiServiceRouter,
    group: &str,
    name: &str,
    definition: &[(&str, &str)],
) -> (u16, String, String) {
    let mut params: Vec<(&str, &str)> = vec![
        ("AutoScalingGroupName", group),
        ("ScheduledActionName", name),
    ];
    params.extend_from_slice(definition);
    let (status, body) = query(router, "PutScheduledUpdateGroupAction", &params).await;
    (
        status,
        tag_value(&body, "Code"),
        xml_unescape(&tag_value(&body, "Message")),
    )
}

/// Post one `PutScheduledUpdateGroupAction` that must be served.
async fn put_served(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    name: &str,
    definition: &[(&str, &str)],
) {
    let (status, code, message) = put_action(router, group, name, definition).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the put of {name:?} must succeed, got {status} with {code:?} \
         {message:?}"
    );
}

/// The three sizes one group reports: `(MinSize, MaxSize, DesiredCapacity)`.
async fn reported_sizes(case: &str, router: &MultiServiceRouter, group: &str) -> (i32, i32, i32) {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: reading {group:?} back expected 200, got {status} with {body}"
    );
    let reported = list_members(&body, "AutoScalingGroups");
    let block = reported.first().unwrap_or_else(|| {
        panic!("case {case:?}: the read reports no group {group:?}: {body}")
    });
    let size = |member: &str| tag_value(block, member).parse::<i32>().unwrap_or(i32::MIN);
    (
        size("MinSize"),
        size("MaxSize"),
        size("DesiredCapacity"),
    )
}

/// Read `group` until it reports `wanted` as its three sizes, or a generous deadline passes —
/// the CONDITION, never a fixed wait. Answers the sizes it last read.
async fn sizes_settling_on(
    case: &str,
    router: &MultiServiceRouter,
    group: &str,
    wanted: (i32, i32, i32),
) -> (i32, i32, i32) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let reported = reported_sizes(case, router, group).await;
        if reported == wanted || Instant::now() >= deadline {
            return reported;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Each scheduled action `DescribeScheduledActions` reports for `group`, as its own block.
async fn reported_actions(case: &str, router: &MultiServiceRouter, group: &str) -> Vec<String> {
    let (status, body) = query(
        router,
        "DescribeScheduledActions",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeScheduledActions({group}) expected 200, got {status} with \
         {body}"
    );
    list_members(&body, "ScheduledUpdateGroupActions")
        .into_iter()
        .map(ToString::to_string)
        .collect()
}

/// The names of the actions `group` still holds, sorted.
async fn reported_names(case: &str, router: &MultiServiceRouter, group: &str) -> Vec<String> {
    let mut names: Vec<String> = reported_actions(case, router, group)
        .await
        .iter()
        .map(|action| tag_value(action, "ScheduledActionName"))
        .collect();
    names.sort();
    names
}

/// One member of the action named `name`, empty when the group no longer holds it.
async fn reported_member(
    case: &str,
    router: &MultiServiceRouter,
    group: &str,
    name: &str,
    member: &str,
) -> String {
    reported_actions(case, router, group)
        .await
        .iter()
        .find(|action| tag_value(action, "ScheduledActionName") == name)
        .map(|action| tag_value(action, member))
        .unwrap_or_default()
}

/// Read `group`'s schedule until the action names it holds are `wanted`, or a generous
/// deadline passes. Answers the names it last read.
async fn names_settling_on(
    case: &str,
    router: &MultiServiceRouter,
    group: &str,
    wanted: &[&str],
) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let names = reported_names(case, router, group).await;
        if names == wanted || Instant::now() >= deadline {
            return names;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One `SuspendProcesses` or `ResumeProcesses` call naming exactly one process.
async fn process_call(router: &MultiServiceRouter, action: &str, group: &str, process: &str) {
    let (status, body) = query(
        router,
        action,
        &[
            ("AutoScalingGroupName", group),
            ("ScalingProcesses.member.1", process),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "{action}({group}, {process}): expected 200, got {status} with {body}"
    );
}

/// One occurrence that must fire: the case, the group and action it is put on, the members the
/// definition carries beyond the instant, and the three sizes the group holds once it has run.
type OccurrenceCase<'a> = (&'a str, &'a str, Vec<(&'a str, &'a str)>, (i32, i32, i32));

/// One action stored to be read back: the case, its name, and the members its definition
/// carries.
type StoredCase<'a> = (&'a str, &'a str, Vec<(&'a str, &'a str)>);

/// One definition the schedule cannot take: the case, the members it carries, and the whole
/// message it is refused with.
type RefusalCase<'a> = (&'a str, Vec<(&'a str, &'a str)>, String);

/// One definition taken against the bounds it states: the case, the members it carries, and
/// the whole message it is refused with — `None` for one the service serves.
type BoundsCase<'a> = (&'a str, Vec<(&'a str, &'a str)>, Option<String>);

/// RULE 1: once the instant an action is timed at has passed, the sizes it states are the
/// group's — whichever of them it states, and however it is timed.
#[tokio::test]
async fn a_due_occurrence_applies_the_sizes_the_action_states() {
    let router = router_with(new_engine());
    let at = instant_ahead(FIRES_IN);

    // The instant is stated by the member named in each row's own definition, so the timing
    // dimension is spanned beside the sizes one: `StartTime`, the legacy `Time` the model
    // calls the same instant, and a `StartTime` with a `Recurrence` beside it.
    let rows: Vec<OccurrenceCase<'_>> = vec![
        (
            "the desired capacity alone, timed by StartTime",
            "due-desired",
            vec![
                ("StartTime", at.as_str()),
                ("DesiredCapacity", "2"),
            ],
            (0, GROUP_MAX_SIZE, 2),
        ),
        (
            // A minimum above the capacity the group holds carries the capacity up to it, the
            // way every other writer of a group's bounds leaves it.
            "the minimum alone, which carries the capacity up to it",
            "due-min",
            vec![("StartTime", at.as_str()), ("MinSize", "1")],
            (1, GROUP_MAX_SIZE, 1),
        ),
        (
            "the maximum alone",
            "due-max",
            vec![("StartTime", at.as_str()), ("MaxSize", "2")],
            (0, 2, 0),
        ),
        (
            "all three sizes at once, timed by the legacy Time member",
            "due-every-size",
            vec![
                ("Time", at.as_str()),
                ("MinSize", "1"),
                ("MaxSize", "3"),
                ("DesiredCapacity", "3"),
            ],
            (1, 3, 3),
        ),
        (
            // "If you specify Recurrence and StartTime, Amazon EC2 Auto Scaling performs the
            // action at this time, and then performs the action based on the specified
            // recurrence": the stated instant is the first occurrence, not merely a boundary.
            "a recurrence beside the instant it starts at",
            "due-recurring",
            vec![
                ("StartTime", at.as_str()),
                ("Recurrence", "0 12 * * *"),
                ("DesiredCapacity", "1"),
            ],
            (0, GROUP_MAX_SIZE, 1),
        ),
    ];

    // Every row is put BEFORE any of them is waited for, so the suite waits once for the one
    // instant they share rather than once per row.
    for (case, name, definition, _) in &rows {
        create_group(&router, name, GROUP_MAX_SIZE).await;
        put_served(&router, case, name, name, definition).await;
    }
    for (case, name, _, expected) in &rows {
        let reported = sizes_settling_on(case, &router, name, *expected).await;
        assert_eq!(
            reported, *expected,
            "case {case:?}: expected the group to hold (min, max, desired) {expected:?} once \
             the occurrence had passed, got {reported:?}"
        );
    }
}

/// RULE 1 (continued): the capacity a due occurrence asks for is capacity the group RUNS, so
/// the instances follow it — through the same path every other capacity change takes.
#[tokio::test]
async fn a_due_occurrence_launches_the_capacity_it_asks_for() {
    let router = router_with(new_engine());
    let group = "due-launches";
    create_group(&router, group, GROUP_MAX_SIZE).await;
    let case = "an occurrence raising the capacity of an empty group";
    put_served(
        &router,
        case,
        group,
        "scale-out",
        &[
            ("StartTime", instant_ahead(FIRES_IN).as_str()),
            ("DesiredCapacity", "2"),
        ],
    )
    .await;

    let expected = (0, GROUP_MAX_SIZE, 2);
    let reported = sizes_settling_on(case, &router, group, expected).await;
    assert_eq!(
        reported, expected,
        "case {case:?}: expected the capacity to move to 2, got {reported:?}"
    );

    // ...and the group holds the instances that capacity is made of, in service.
    let deadline = Instant::now() + DEADLINE;
    let members = loop {
        let (status, body) = query(
            &router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", group)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: reading {group:?} back expected 200, got {status} with {body}"
        );
        let in_service = crate::support::tag_values(&body, "LifecycleState")
            .into_iter()
            .filter(|state| state == "InService")
            .count();
        if in_service == 2 || Instant::now() >= deadline {
            break in_service;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    };
    assert_eq!(
        members, 2,
        "case {case:?}: expected the two instances the capacity is made of to be in service, \
         got {members}"
    );

    // ...and the group's activity history says WHICH schedule moved it: the execution of the
    // occurrence is one step of the group's own history, naming the action, and the cause of
    // every step the occurrence took names it too — so a client reading the history can tell
    // one occurrence of a schedule from any other change of the group.
    let (status, body) = query(
        &router,
        "DescribeScalingActivities",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: reading the activities of {group:?} expected 200, got {status} with \
         {body}"
    );
    let descriptions = crate::support::tag_values(&body, "Description");
    let executions: Vec<&String> = descriptions
        .iter()
        .filter(|description| description.contains("scale-out"))
        .collect();
    assert_eq!(
        executions,
        vec!["Executing scheduled action scale-out"],
        "case {case:?}: expected exactly one activity for the execution of the action, got \
         {descriptions:?}"
    );
    let causes = crate::support::activity_causes(&router, group).await;
    assert!(
        !causes.is_empty()
            && causes.iter().all(|cause| cause
                .contains("a scheduled action scale-out update of AutoScalingGroup constraints")),
        "case {case:?}: expected every activity the occurrence recorded to be caused by the \
         scheduled action's change of the group's constraints, got {causes:?}"
    );
}

/// RULE 2: a group that has suspended its `ScheduledActions` process skips the occurrence —
/// and resuming the process does not bring it back.
#[tokio::test]
async fn a_suspended_group_skips_the_occurrence_and_resuming_does_not_backfill_it() {
    let router = router_with(new_engine());
    let control = "skip-control";
    let suspended = "skip-suspended";
    create_group(&router, control, GROUP_MAX_SIZE).await;
    create_group(&router, suspended, GROUP_MAX_SIZE).await;

    // Suspended BEFORE the action is stored, so no occurrence can slip through ahead of the
    // suspension however slowly the machine runs.
    process_call(&router, "SuspendProcesses", suspended, "ScheduledActions").await;

    let at = instant_ahead(FIRES_IN);
    for group in [control, suspended] {
        put_served(
            &router,
            "the same occurrence on both groups",
            group,
            "nightly",
            &[("StartTime", at.as_str()), ("DesiredCapacity", "2")],
        )
        .await;
    }

    // The control group — the one running the process — performs it.
    let case = "the group running the process";
    let expected = (0, GROUP_MAX_SIZE, 2);
    let reported = sizes_settling_on(case, &router, control, expected).await;
    assert_eq!(
        reported, expected,
        "case {case:?}: expected the occurrence to be performed, got {reported:?}"
    );

    // The suspended group's occurrence was SKIPPED: its sizes did not move, and the action
    // that stated them is spent, so nothing is left for a later instant to perform.
    let case = "the group that has suspended the process";
    let skipped = (0, GROUP_MAX_SIZE, 0);
    let names = names_settling_on(case, &router, suspended, &[]).await;
    assert!(
        names.is_empty(),
        "case {case:?}: expected the skipped occurrence to leave no pending action, got \
         {names:?}"
    );
    let reported = reported_sizes(case, &router, suspended).await;
    assert_eq!(
        reported, skipped,
        "case {case:?}: expected the group's sizes to be untouched, got {reported:?}"
    );
    // The skip is a step of the group's history: the only signal a client has that the
    // suspension, rather than a schedule that never fired, is why the group did not move.
    let (status, body) = query(
        &router,
        "DescribeScalingActivities",
        &[("AutoScalingGroupName", suspended)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: reading the activities of {suspended:?} expected 200, got {status} \
         with {body}"
    );
    let recorded: Vec<(String, String)> = list_members(&body, "Activities")
        .into_iter()
        .map(|activity| {
            (
                tag_value(activity, "Description"),
                tag_value(activity, "StatusCode"),
            )
        })
        .collect();
    assert_eq!(
        recorded,
        vec![(
            "Skipped scheduled action nightly".to_string(),
            "Cancelled".to_string()
        )],
        "case {case:?}: expected exactly one skipped occurrence and no execution, got \
         {recorded:?}"
    );

    // Resuming the process has the group perform the occurrences that fall due from then on,
    // and never the one that fell due while it was suspended: there is no action left to
    // perform, so the sizes cannot move again.
    process_call(&router, "ResumeProcesses", suspended, "ScheduledActions").await;
    let case = "the same group after the process was resumed";
    let names = reported_names(case, &router, suspended).await;
    assert!(
        names.is_empty(),
        "case {case:?}: expected the resumption to bring back no missed occurrence, got \
         {names:?}"
    );
    let reported = reported_sizes(case, &router, suspended).await;
    assert_eq!(
        reported, skipped,
        "case {case:?}: expected the group's sizes to stay where they were, got {reported:?}"
    );
}

/// RULE 3: a spent action and one whose window has elapsed stop being reported; a pending one
/// and a recurring one are still scheduled, the recurring one at the occurrence that follows
/// the one it ran.
#[tokio::test]
async fn an_action_is_reported_only_while_it_is_still_scheduled() {
    let router = router_with(new_engine());
    let group = "retention";
    create_group(&router, group, GROUP_MAX_SIZE).await;

    let fires_at = instant_ahead(FIRES_IN);
    let window_closes_at = instant_ahead(FIRES_IN + Duration::from_secs(1));
    let stays_pending_at = instant_ahead(STAYS_PENDING_IN);

    // (case, the action name, the definition)
    let rows: Vec<StoredCase<'_>> = vec![
        (
            "a one-time action that runs",
            "one-time",
            vec![
                ("StartTime", fires_at.as_str()),
                ("DesiredCapacity", "1"),
            ],
        ),
        (
            "an action whose window opens and then closes",
            "windowed",
            vec![
                ("StartTime", fires_at.as_str()),
                ("EndTime", window_closes_at.as_str()),
                ("MaxSize", "3"),
            ],
        ),
        (
            "an action whose occurrence is still ahead",
            "pending",
            vec![
                ("StartTime", stays_pending_at.as_str()),
                ("DesiredCapacity", "1"),
            ],
        ),
        (
            "a recurring action that runs and recurs",
            "recurring",
            vec![
                ("StartTime", fires_at.as_str()),
                ("Recurrence", "0 12 * * *"),
                ("MaxSize", "3"),
            ],
        ),
    ];
    for (case, name, definition) in &rows {
        put_served(&router, case, group, name, definition).await;
    }
    // The recurring action reports the instant it next runs at, which is the one it was timed
    // with until that occurrence has passed.
    let before = reported_member("the recurring action", &router, group, "recurring", "StartTime")
        .await;
    assert_eq!(
        before, fires_at,
        "expected the recurring action to be scheduled at {fires_at:?}, got {before:?}"
    );

    // Once the window has closed, exactly the two actions that are still scheduled are
    // reported: the one still waiting for its instant, and the one that recurs.
    let case = "the schedule once the occurrences have passed";
    let names = names_settling_on(case, &router, group, &["pending", "recurring"]).await;
    assert_eq!(
        names,
        vec!["pending".to_string(), "recurring".to_string()],
        "case {case:?}: expected the spent and the elapsed action to drop out, got {names:?}"
    );

    // ...and the recurring one has moved on to the occurrence AFTER the one it ran.
    let after = reported_member(case, &router, group, "recurring", "StartTime").await;
    assert!(
        after > before,
        "case {case:?}: expected the recurring action to report the next occurrence after \
         {before:?}, got {after:?}"
    );
}

/// RULE 4: a sub-second fraction a request states is truncated to whole seconds on write.
///
/// The rows span the fraction, including the one that would round UP to the next second: a
/// truncation drops it, so every row reports the same whole second.
#[tokio::test]
async fn an_instant_a_put_states_is_held_to_whole_seconds() {
    let router = router_with(new_engine());
    let group = "truncation";
    create_group(&router, group, GROUP_MAX_SIZE).await;

    // (case, the fraction the request writes after the seconds)
    let rows: &[(&str, &str)] = &[
        ("no fraction at all", ""),
        ("a microsecond fraction", ".123456"),
        ("a fraction that would round up", ".999999"),
        ("half a second", ".5"),
        ("the smallest fraction a microsecond states", ".000001"),
        ("a fraction of zeroes", ".000000"),
    ];

    for (index, (case, fraction)) in rows.iter().enumerate() {
        let name = format!("truncated-{index}");
        let start_time = format!("2035-06-01T12:00:00{fraction}Z");
        let end_time = format!("2035-07-01T00:00:00{fraction}Z");
        put_served(
            &router,
            case,
            group,
            &name,
            &[
                ("StartTime", start_time.as_str()),
                ("EndTime", end_time.as_str()),
                ("DesiredCapacity", "1"),
            ],
        )
        .await;

        // `Time` is the legacy spelling of the one instant the action starts at, so it is
        // held to the same whole second.
        for (member, expected) in [
            ("StartTime", "2035-06-01T12:00:00Z"),
            ("Time", "2035-06-01T12:00:00Z"),
            ("EndTime", "2035-07-01T00:00:00Z"),
        ] {
            let reported = reported_member(case, &router, group, &name, member).await;
            assert_eq!(
                reported, expected,
                "case {case:?}: expected {member} {expected:?}, got {reported:?}"
            );
        }
    }
}

/// RULE 5: what the schedule cannot take is refused with HTTP 400 `ValidationError` before the
/// sizes are weighed, and nothing is stored.
///
/// The group is created with no room at all — minimum and maximum both zero — so a capacity of
/// one is outside its bounds: a row that is nevertheless refused for its SCHEDULE proves the
/// schedule is read first.
#[tokio::test]
async fn a_schedule_the_service_cannot_take_is_refused_before_the_sizes_are_weighed() {
    let router = router_with(new_engine());
    let group = "refused-schedule";
    create_group(&router, group, 0).await;

    let a_second_ago = instant_behind(Duration::from_secs(1));
    let an_hour_ago = instant_behind(Duration::from_secs(60 * 60));
    let a_year_ago = instant_behind(Duration::from_secs(365 * 24 * 60 * 60));

    // (case, the definition, the whole message it is refused with)
    let rows: Vec<RefusalCase<'_>> = vec![
        (
            "a start time a second behind the clock",
            vec![
                ("StartTime", a_second_ago.as_str()),
                ("DesiredCapacity", "0"),
            ],
            START_TIME_IN_PAST_MESSAGE.to_string(),
        ),
        (
            "a start time an hour behind the clock",
            vec![("StartTime", an_hour_ago.as_str()), ("MinSize", "0")],
            START_TIME_IN_PAST_MESSAGE.to_string(),
        ),
        (
            "a start time a year behind the clock",
            vec![("StartTime", a_year_ago.as_str()), ("MaxSize", "0")],
            START_TIME_IN_PAST_MESSAGE.to_string(),
        ),
        (
            "an instant behind the clock, named by the legacy Time member",
            vec![("Time", an_hour_ago.as_str()), ("MaxSize", "0")],
            START_TIME_IN_PAST_MESSAGE.to_string(),
        ),
        (
            "a recurrence that is prose rather than a crontab expression",
            vec![
                ("Recurrence", "not a cron expression"),
                ("MaxSize", "0"),
                ("DesiredCapacity", "1"),
            ],
            invalid_recurrence_message("not a cron expression"),
        ),
        (
            "a recurrence one field short",
            vec![
                ("Recurrence", "0 0 * *"),
                ("MaxSize", "0"),
                ("DesiredCapacity", "1"),
            ],
            invalid_recurrence_message("0 0 * *"),
        ),
        (
            "a recurrence whose minute is not a minute",
            vec![("Recurrence", "sixty 0 * * *"), ("DesiredCapacity", "1")],
            invalid_recurrence_message("sixty 0 * * *"),
        ),
        (
            // The six-field `cron(…)` body other AWS services take is not this grammar.
            "a six-field AWS cron body",
            vec![("Recurrence", "0 10 * * ? *"), ("DesiredCapacity", "1")],
            invalid_recurrence_message("0 10 * * ? *"),
        ),
    ];

    for (case, definition, expected) in &rows {
        let (status, code, message) = put_action(&router, group, "refused", definition).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {code:?} {message:?}"
        );
        assert_eq!(
            code, "ValidationError",
            "case {case:?}: expected the ValidationError code, got {code:?} {message:?}"
        );
        assert_eq!(
            &message, expected,
            "case {case:?}: expected the message {expected:?}, got {message:?}"
        );
    }

    // The capacity refusal the two rows carrying a stated maximum must NOT have been
    // answered with: the same sizes on a schedule the service CAN take are refused for the
    // capacity, which is what makes the schedule's precedence observable rather than
    // incidental.
    let case = "a capacity outside the maximum the action states, on a schedule it can take";
    let (status, code, message) = put_action(
        &router,
        group,
        "refused",
        &[
            ("StartTime", instant_ahead(STAYS_PENDING_IN).as_str()),
            ("MaxSize", "0"),
            ("DesiredCapacity", "1"),
        ],
    )
    .await;
    assert_eq!(
        (status, code.as_str(), message.as_str()),
        (400, "ValidationError", desired_capacity_message(1, 0, 0).as_str()),
        "case {case:?}: unexpected refusal"
    );

    // Nothing any refused row stated reached the store.
    let names = reported_names("the group after every refusal", &router, group).await;
    assert!(
        names.is_empty(),
        "expected a refused put to store nothing, got {names:?}"
    );
}

/// RULE 5 (continued): a put is taken against the bounds the ACTION states, and not against
/// the ones it leaves to the group — the occurrence runs later, when the group's own bounds
/// may be anything, and the run is what brings the capacity inside them.
///
/// The rows span WHICH bound the action states: both, the one the capacity breaks, and
/// neither.
#[tokio::test]
async fn a_put_is_taken_against_the_bounds_the_action_itself_states() {
    let router = router_with(new_engine());
    let group = "stated-bounds";
    // Room for one instance, so a capacity of three is outside the bounds the group holds
    // now while being inside none the action states.
    create_group(&router, group, 1).await;
    let at = instant_ahead(STAYS_PENDING_IN);

    // (case, the definition, the whole message it is refused with, or None when it is served)
    let rows: Vec<BoundsCase<'_>> = vec![
        (
            "a capacity below the minimum the action itself states",
            vec![
                ("StartTime", at.as_str()),
                ("MinSize", "2"),
                ("DesiredCapacity", "1"),
            ],
            Some(desired_capacity_message(1, 2, 1)),
        ),
        (
            "a capacity above the maximum the action itself states",
            vec![
                ("StartTime", at.as_str()),
                ("MaxSize", "2"),
                ("DesiredCapacity", "3"),
            ],
            Some(desired_capacity_message(3, 0, 2)),
        ),
        (
            "a maximum below the minimum, both stated by the action",
            vec![
                ("StartTime", at.as_str()),
                ("MinSize", "5"),
                ("MaxSize", "1"),
            ],
            Some(max_below_min_message(1, 5)),
        ),
        (
            "a capacity outside the bounds the action leaves to the group",
            vec![("StartTime", at.as_str()), ("DesiredCapacity", "3")],
            None,
        ),
        (
            "a minimum above the maximum the action leaves to the group",
            vec![("StartTime", at.as_str()), ("MinSize", "5")],
            None,
        ),
    ];

    for (index, (case, definition, expected)) in rows.iter().enumerate() {
        let name = format!("stated-{index}");
        let (status, code, message) = put_action(&router, group, &name, definition).await;
        match expected {
            Some(expected) => {
                assert_eq!(
                    (status, code.as_str(), message.as_str()),
                    (400, "ValidationError", expected.as_str()),
                    "case {case:?}: expected the refusal {expected:?}, got {status} \
                     {code:?} {message:?}"
                );
            }
            None => assert_eq!(
                status, 200,
                "case {case:?}: expected the put to be served, got {status} with {code:?} \
                 {message:?}"
            ),
        }
    }

    // ...and the capacity the group could not hold is brought inside the bounds in force
    // when the occurrence runs, rather than refused when it was stored.
    let case = "an occurrence whose capacity is above the group's own maximum";
    let fires = "clamped";
    put_served(
        &router,
        case,
        group,
        fires,
        &[
            ("StartTime", instant_ahead(FIRES_IN).as_str()),
            ("DesiredCapacity", "3"),
        ],
    )
    .await;
    let expected = (0, 1, 1);
    let reported = sizes_settling_on(case, &router, group, expected).await;
    assert_eq!(
        reported, expected,
        "case {case:?}: expected the run to bring the capacity inside the group's bounds, \
         got {reported:?}"
    );
}
