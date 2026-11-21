// SPDX-License-Identifier: CC0-1.0

//! The scheduled scaling actions of an Auto Scaling group, over the awsQuery wire: what
//! `PutScheduledUpdateGroupAction` creates or updates and `DescribeScheduledActions` reports
//! back.
//!
//! A scheduled action sets the same three sizes a scaling policy's execution sets, without a
//! metric and without a request per change — the sibling of the policies the parent suite
//! pins — so it is the parent suite's own concern, in its own file.
//!
//! The rules under test:
//!
//! 1. A put against a stored group states the action WHOLE — when it runs and which of the
//!    group's sizes it sets — and the action is then reported by `DescribeScheduledActions`
//!    carrying exactly those members, its group, and the ARN minted for it. `Time` "is no
//!    longer used" (`PutScheduledUpdateGroupActionType$Time`), so it and `StartTime` name
//!    ONE instant and a read reports it under both.
//! 2. An action the put did not time itself but gave a `Recurrence` reports WHEN it next
//!    runs: the next occurrence of that crontab expression, read in the zone `TimeZone`
//!    names and in UTC when it names none, resolved once when the action is stored — so two
//!    reads report the same instant.
//! 3. Putting the same action name again REPLACES that definition and keeps the identity
//!    minted when it was created.
//! 4. A definition that cannot be scheduled is refused with HTTP 400 `ValidationError`
//!    carrying the sentence the service states for that class, and NOTHING is stored.
//! 5. The read selects by the group, by the action names ("if you specify an unknown
//!    scheduled action, it is ignored with no error") and by the start-time window ("if
//!    scheduled action names are provided, this property is ignored"), and pages its answer.
//! 6. A scheduled action only exists against a group that exists: deleting the group deletes
//!    its actions.
//!
//! Each table spans what its rule generalises over (how the action is timed, which sizes it
//! sets, which zone a recurrence is read in, which selection the read carries, which part of
//! a definition cannot be scheduled) rather than one recorded row, every expected sentence
//! is written out here rather than taken from the crate, and every assertion names its case
//! and the value actually reported.

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    borrowed, list_members, max_below_min_message, new_engine, p, query, router_with, tag_value,
    tag_values, xml_unescape, REGION,
};

/// The launch source every group in this suite is created from — a launch TEMPLATE, because
/// no account can create a launch configuration (see
/// `launch_configurations_are_unavailable.rs`).
const LAUNCH_SOURCE: (&str, &str) = ("LaunchTemplate.LaunchTemplateName", "lt-1");

/// Where every group in this suite places the instances it would launch.
const ZONE: (&str, &str) = ("AvailabilityZones.member.1", "us-west-2a");

/// The bounds every group here is created with, which the size rules below are taken
/// against.
const GROUP_MIN_SIZE: i32 = 0;
const GROUP_MAX_SIZE: i32 = 3;

/// The most instances this process holds one group to, and the most scheduled actions one
/// group may hold — the two ceilings the operation answers its modelled `LimitExceededFault`
/// for. Written out here rather than taken from the crate, so the assertion does not take the
/// handler's own word for either.
const MAX_GROUP_CAPACITY: i32 = 1_000;
const MAX_SCHEDULED_ACTIONS_PER_GROUP: usize = 125;

/// The whole message a put stating none of the three sizes is refused with: the rule as the
/// member's own documentation states it.
const SIZE_REQUIRED_MESSAGE: &str =
    "You must specify at least one of the following properties: MaxSize, MinSize, or \
     DesiredCapacity";

/// The whole message a put whose window does not open is refused with.
const END_TIME_MESSAGE: &str = "Given end time must be after start time";

/// The whole message a put whose `Recurrence` is not the documented crontab expression is
/// refused with: the sentence this service states for that class, quoting the rejected value.
fn invalid_recurrence_message(recurrence: &str) -> String {
    format!("Given recurrence string: {recurrence} is invalid")
}

/// The whole message a put whose `TimeZone` is not an IANA name is refused with: entirely
/// fixed text, with no part of the request in it — the rejected zone name is not quoted
/// back, so every unknown zone reads the same.
const INVALID_TIME_ZONE_MESSAGE: &str =
    "Time zone specified does not conform to standardized time zones found in IANA Time Zone \
     Database";

/// The whole message a put whose sizes the group could not hold is refused with: the
/// sentence every writer of a group's three sizes states.
fn desired_capacity_message(desired_capacity: i32, min_size: i32, max_size: i32) -> String {
    format!(
        "Desired capacity:{desired_capacity} must be between the specified min \
         size:{min_size} and max size:{max_size}"
    )
}

/// The whole message `DescribeScheduledActions` refuses a group the account does not hold
/// with: the absent-group prefix this service opens every such refusal with, and the detail
/// form that read serves — the group named, unquoted.
fn absent_group_message(name: &str) -> String {
    format!("AutoScalingGroup name not found - AutoScalingGroup {name} not found")
}

/// The whole message `PutScheduledUpdateGroupAction` refuses such a group with: the same
/// prefix with the detail field left unfilled, so it travels as the literal token `null` —
/// the refusal's subject is the group the request named, which is the detail form this
/// WRITE serves.
const ABSENT_GROUP_WITHOUT_DETAIL: &str = "AutoScalingGroup name not found - null";

/// A router carrying Amazon EC2 Auto Scaling on one engine whose Amazon EC2 store already
/// holds the launch templates these groups launch from.
fn router() -> MultiServiceRouter {
    router_with(new_engine())
}

/// Create one empty group (nothing to launch, so nothing settles on the clock while the
/// actions are written and read).
async fn create_group(router: &MultiServiceRouter, name: &str) {
    let (status, body) = query(
        router,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", name),
            LAUNCH_SOURCE,
            ("MinSize", "0"),
            ("MaxSize", "3"),
            ("DesiredCapacity", "0"),
            ZONE,
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "creating {name:?} must succeed, got {status} with {body:?}"
    );
}

/// A router with the group `name` created on it.
async fn given_group(name: &str) -> MultiServiceRouter {
    let router = router();
    create_group(&router, name).await;
    router
}

/// Post one `PutScheduledUpdateGroupAction` stating `definition` beyond the two names, and
/// answer its status, wire error code and message.
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

/// Post one `DescribeScheduledActions` carrying `selection`, and answer its status, wire
/// error code and message beside the body.
async fn describe_result(
    router: &MultiServiceRouter,
    selection: &[(String, String)],
) -> (u16, String, String, String) {
    let (status, body) = query(router, "DescribeScheduledActions", &borrowed(selection)).await;
    (
        status,
        tag_value(&body, "Code"),
        xml_unescape(&tag_value(&body, "Message")),
        body,
    )
}

/// Post one `DescribeScheduledActions` that must be served, and answer its body.
async fn describe(
    router: &MultiServiceRouter,
    case: &str,
    selection: &[(String, String)],
) -> String {
    let (status, code, message, body) = describe_result(router, selection).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the read must succeed, got {status} with {code:?} {message:?}"
    );
    body
}

/// Each scheduled action one answer reports, as its own `<member>` block: the answer's
/// `ScheduledUpdateGroupActions` list, read through the harness's own list reader so a member
/// is read inside the action it belongs to.
fn reported_actions(xml: &str) -> Vec<&str> {
    list_members(xml, "ScheduledUpdateGroupActions")
}

/// The names of the actions one answer reports, in the order it reports them.
fn reported_names(xml: &str) -> Vec<String> {
    reported_actions(xml)
        .into_iter()
        .map(|action| tag_value(action, "ScheduledActionName"))
        .collect()
}

/// The block of the action named `name`, or `None` when the answer reports no such action.
fn action_named<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    reported_actions(xml)
        .into_iter()
        .find(|action| tag_value(action, "ScheduledActionName") == name)
}

/// One member of a reported action, `None` when the answer omits it — which is what tells a
/// member the definition does not carry from one it does.
fn reported_member(action: &str, member: &str) -> Option<String> {
    tag_values(action, member).into_iter().next()
}

/// The members one request carries beyond the names, as the owned pairs the harness takes.
type Params = Vec<(String, String)>;

/// One definition a put states, as the borrowed pairs a row writes it with.
type Definition<'a> = Vec<(&'a str, &'a str)>;

/// One definition a put states: the case, the action name, the members it carries, the
/// members the read-back must report and the members it must NOT report.
type DefinitionCase<'a> = (
    &'a str,
    &'a str,
    Definition<'a>,
    Vec<(&'a str, &'a str)>,
    Vec<&'a str>,
);

/// One recurring definition: the case, the action name, the members it carries, and the UTC
/// time of day its next occurrence falls at.
type RecurrenceCase<'a> = (&'a str, &'a str, Definition<'a>, &'a str);

/// One definition that cannot be scheduled: the case, the group the put names, the members it
/// carries, and the whole message it is refused with.
type PutRefusalCase<'a> = (&'a str, &'a str, Definition<'a>, String);

/// One selection the read carries: the case, the selection, and the action names it must
/// report.
type SelectionCase<'a> = (&'a str, Params, Vec<&'a str>);

/// One selection the read cannot serve: the case, the selection, and the wire error code and
/// whole message it is refused with.
type ReadRefusalCase<'a> = (&'a str, Params, &'a str, String);

/// RULE 1: a put states the action whole, and a read reports exactly what it stated — under
/// both spellings of the one instant it starts at — beside the group it belongs to and the
/// ARN minted for it.
#[tokio::test]
async fn a_put_states_the_action_a_read_reports() {
    let group = "scheduled-actions";
    let router = given_group(group).await;

    let rows: Vec<DefinitionCase<'_>> = vec![
        (
            "timed by StartTime, setting the desired capacity",
            "one-off-start-time",
            vec![
                ("StartTime", "2030-06-01T12:00:00Z"),
                ("DesiredCapacity", "2"),
            ],
            vec![
                ("StartTime", "2030-06-01T12:00:00Z"),
                ("Time", "2030-06-01T12:00:00Z"),
                ("DesiredCapacity", "2"),
            ],
            vec!["MinSize", "MaxSize", "Recurrence", "EndTime", "TimeZone"],
        ),
        (
            "timed by the legacy Time member, setting the minimum size",
            "one-off-legacy-time",
            vec![("Time", "2031-01-02T03:04:05Z"), ("MinSize", "1")],
            vec![
                ("StartTime", "2031-01-02T03:04:05Z"),
                ("Time", "2031-01-02T03:04:05Z"),
                ("MinSize", "1"),
            ],
            vec!["DesiredCapacity", "MaxSize", "Recurrence", "EndTime"],
        ),
        (
            "setting every size at once, inside a window",
            "every-size",
            vec![
                ("StartTime", "2030-06-01T00:00:00Z"),
                ("EndTime", "2030-07-01T00:00:00Z"),
                ("MinSize", "1"),
                ("MaxSize", "3"),
                ("DesiredCapacity", "2"),
            ],
            vec![
                ("StartTime", "2030-06-01T00:00:00Z"),
                ("EndTime", "2030-07-01T00:00:00Z"),
                ("MinSize", "1"),
                ("MaxSize", "3"),
                ("DesiredCapacity", "2"),
            ],
            vec!["Recurrence", "TimeZone"],
        ),
        (
            "setting only the maximum size",
            "max-size-only",
            vec![("StartTime", "2032-03-04T05:06:07Z"), ("MaxSize", "3")],
            vec![("MaxSize", "3"), ("Time", "2032-03-04T05:06:07Z")],
            vec!["MinSize", "DesiredCapacity"],
        ),
        (
            "a recurrence bounded by a window, keeping the zone it is read in",
            "bounded-recurrence",
            vec![
                ("Recurrence", "0 12 * * *"),
                ("EndTime", "2040-01-01T00:00:00Z"),
                ("TimeZone", "Pacific/Tahiti"),
                ("MinSize", "1"),
                ("MaxSize", "3"),
            ],
            vec![
                ("Recurrence", "0 12 * * *"),
                ("EndTime", "2040-01-01T00:00:00Z"),
                ("TimeZone", "Pacific/Tahiti"),
                ("MinSize", "1"),
                ("MaxSize", "3"),
            ],
            vec!["DesiredCapacity"],
        ),
    ];

    for (case, name, definition, reported, absent) in &rows {
        put_served(&router, case, group, name, definition).await;

        let answer = describe(
            &router,
            case,
            &[
                p("AutoScalingGroupName", group),
                p("ScheduledActionNames.member.1", *name),
            ],
        )
        .await;
        let action = action_named(&answer, name).unwrap_or_else(|| {
            panic!("case {case:?}: the read reports no action {name:?}: {answer}")
        });
        for (member, expected) in reported {
            let value = reported_member(action, member).unwrap_or_default();
            assert_eq!(
                &value, expected,
                "case {case:?}: expected {member} {expected:?}, got {value:?} in {action}"
            );
        }
        for member in absent {
            assert!(
                reported_member(action, member).is_none(),
                "case {case:?}: expected no {member}, got {action}"
            );
        }
        let group_reported = reported_member(action, "AutoScalingGroupName").unwrap_or_default();
        assert_eq!(
            group_reported, group,
            "case {case:?}: expected the action to report its group {group:?}, got \
             {group_reported:?}"
        );
        // The ARN of a scheduled action names the account, the Region, this group and this
        // action; the id between them is the one the service minted for it.
        let arn = reported_member(action, "ScheduledActionARN").unwrap_or_default();
        let head = format!(
            "arn:aws:autoscaling:{REGION}:{}:scheduledUpdateGroupAction:",
            ::engine::emulator_account_id()
        );
        let tail = format!(":autoScalingGroupName/{group}:scheduledActionName/{name}");
        assert!(
            arn.starts_with(&head) && arn.ends_with(&tail),
            "case {case:?}: expected an ARN opening {head:?} and ending {tail:?}, got {arn:?}"
        );
    }

    // Every stored action is reported, in name order, when the read selects none: nothing a
    // row stored was dropped and nothing was invented.
    let answer = describe(
        &router,
        "every action of the group",
        &[p("AutoScalingGroupName", group)],
    )
    .await;
    let mut expected: Vec<String> = rows
        .iter()
        .map(|(_, name, ..)| (*name).to_string())
        .collect();
    expected.sort();
    let names = reported_names(&answer);
    assert_eq!(
        names, expected,
        "expected the stored actions {expected:?}, got {names:?} in {answer}"
    );
}

/// RULE 2: a purely recurring action reports the next occurrence of its own expression, in
/// the zone it is read in, and reading it does not move it.
///
/// The DATE such an occurrence falls on depends on when the suite runs; its time of day does
/// not — that is a fact about the expression and the zone. `Etc/GMT+9` is UTC-09:00 in the
/// IANA naming the member's own documentation cites, so 04:17 there is 13:17 UTC.
#[tokio::test]
async fn a_recurring_action_reports_its_next_occurrence() {
    let group = "recurring-actions";
    let router = given_group(group).await;

    // (case, action name, the definition, the UTC time of day its next occurrence falls at)
    let rows: &[RecurrenceCase<'_>] = &[
        (
            "a daily recurrence, read in UTC by default",
            "daily-utc",
            vec![("Recurrence", "17 4 * * *"), ("DesiredCapacity", "1")],
            "T04:17:00Z",
        ),
        (
            "the same recurrence read in a zone behind UTC",
            "daily-behind",
            vec![
                ("Recurrence", "17 4 * * *"),
                ("TimeZone", "Etc/GMT+9"),
                ("DesiredCapacity", "1"),
            ],
            "T13:17:00Z",
        ),
        (
            "the same recurrence read in a zone ahead of UTC",
            "daily-ahead",
            vec![
                ("Recurrence", "17 4 * * *"),
                ("TimeZone", "Etc/GMT-9"),
                ("DesiredCapacity", "1"),
            ],
            "T19:17:00Z",
        ),
        (
            "a recurrence naming a day of the week in the crontab numbering",
            "weekly",
            vec![("Recurrence", "0 6 * * 5"), ("MaxSize", "3")],
            "T06:00:00Z",
        ),
        (
            "a recurrence naming a day of the month and a month",
            "yearly",
            vec![("Recurrence", "30 0 1 1,6,12 *"), ("MinSize", "1")],
            "T00:30:00Z",
        ),
    ];

    for (case, name, definition, time_of_day) in rows {
        put_served(&router, case, group, name, definition).await;

        let selection = [
            p("AutoScalingGroupName", group),
            p("ScheduledActionNames.member.1", *name),
        ];
        let answer = describe(&router, case, &selection).await;
        let action = action_named(&answer, name).unwrap_or_else(|| {
            panic!("case {case:?}: the read reports no action {name:?}: {answer}")
        });
        let start_time = reported_member(action, "StartTime").unwrap_or_default();
        assert!(
            start_time.ends_with(time_of_day),
            "case {case:?}: expected the next occurrence at {time_of_day:?}, got \
             {start_time:?} in {action}"
        );
        let legacy = reported_member(action, "Time").unwrap_or_default();
        assert_eq!(
            legacy, start_time,
            "case {case:?}: expected the legacy Time to name the same instant as StartTime \
             ({start_time:?}), got {legacy:?}"
        );
        // Resolved when the action was stored: a second read reports the same instant.
        let again = describe(&router, case, &selection).await;
        let read_again = action_named(&again, name)
            .and_then(|action| reported_member(action, "StartTime"))
            .unwrap_or_default();
        assert_eq!(
            read_again, start_time,
            "case {case:?}: expected a second read to report {start_time:?}, got \
             {read_again:?}"
        );
    }
}

/// RULE 3: a second put of one name replaces that definition and keeps the ARN minted when
/// the action was created.
#[tokio::test]
async fn a_second_put_of_one_name_replaces_the_definition_and_keeps_its_arn() {
    let group = "upserted-actions";
    let name = "upserted";
    let router = given_group(group).await;

    put_served(
        &router,
        "the create",
        group,
        name,
        &[
            ("Recurrence", "0 1 * * *"),
            ("MinSize", "1"),
            ("MaxSize", "3"),
            ("DesiredCapacity", "2"),
        ],
    )
    .await;
    let created = describe(
        &router,
        "the created action",
        &[p("AutoScalingGroupName", group)],
    )
    .await;
    let arn = action_named(&created, name)
        .and_then(|action| reported_member(action, "ScheduledActionARN"))
        .unwrap_or_default();
    assert!(
        !arn.is_empty(),
        "the created action must report an ARN, got {created}"
    );

    // The same name again, stating a different action WHOLE: a one-off with one size.
    put_served(
        &router,
        "the update",
        group,
        name,
        &[("StartTime", "2033-08-09T10:11:12Z"), ("MaxSize", "3")],
    )
    .await;
    let updated_answer = describe(
        &router,
        "the updated action",
        &[p("AutoScalingGroupName", group)],
    )
    .await;
    let names = reported_names(&updated_answer);
    assert_eq!(
        names,
        vec![name.to_string()],
        "expected the group to hold one action after the update, got {names:?}"
    );
    let updated = action_named(&updated_answer, name)
        .unwrap_or_else(|| panic!("the read reports no action {name:?}: {updated_answer}"));
    let updated_arn = reported_member(updated, "ScheduledActionARN").unwrap_or_default();
    assert_eq!(
        updated_arn, arn,
        "expected the update to keep the ARN {arn:?}, got {updated_arn:?}"
    );
    for (member, expected) in [
        ("StartTime", "2033-08-09T10:11:12Z"),
        ("Time", "2033-08-09T10:11:12Z"),
        ("MaxSize", "3"),
    ] {
        let value = reported_member(updated, member).unwrap_or_default();
        assert_eq!(
            value, expected,
            "expected the updated {member} {expected:?}, got {value:?} in {updated}"
        );
    }
    for member in ["MinSize", "DesiredCapacity", "Recurrence"] {
        assert!(
            reported_member(updated, member).is_none(),
            "expected the update to replace the definition, so no {member}: {updated}"
        );
    }
}

/// RULE 4: a definition that cannot be scheduled is refused, and nothing is stored.
#[tokio::test]
async fn a_definition_that_cannot_be_scheduled_is_refused_and_stores_nothing() {
    let group = "refused-actions";
    let absent = "no-such-group";
    let router = given_group(group).await;

    // (case, the group the put names, the definition, the whole message it is refused with)
    let rows: Vec<PutRefusalCase<'_>> = vec![
        (
            "no size at all",
            group,
            vec![("StartTime", "2030-06-01T12:00:00Z")],
            SIZE_REQUIRED_MESSAGE.to_string(),
        ),
        (
            "no member but the two names",
            group,
            vec![],
            SIZE_REQUIRED_MESSAGE.to_string(),
        ),
        (
            "an end before the start",
            group,
            vec![
                ("StartTime", "2030-06-01T12:00:00Z"),
                ("EndTime", "2030-05-01T12:00:00Z"),
                ("DesiredCapacity", "1"),
            ],
            END_TIME_MESSAGE.to_string(),
        ),
        (
            "an end at the start",
            group,
            vec![
                ("StartTime", "2030-06-01T12:00:00Z"),
                ("EndTime", "2030-06-01T12:00:00Z"),
                ("DesiredCapacity", "1"),
            ],
            END_TIME_MESSAGE.to_string(),
        ),
        (
            "an end before the start the legacy Time member named",
            group,
            vec![
                ("Time", "2030-06-01T12:00:00Z"),
                ("EndTime", "2030-05-01T12:00:00Z"),
                ("DesiredCapacity", "1"),
            ],
            END_TIME_MESSAGE.to_string(),
        ),
        (
            "a recurrence one field short",
            group,
            vec![("Recurrence", "0 0 * *"), ("DesiredCapacity", "1")],
            invalid_recurrence_message("0 0 * *"),
        ),
        (
            "a recurrence that is prose rather than a cron expression",
            group,
            vec![
                ("Recurrence", "not a cron expression"),
                ("DesiredCapacity", "1"),
            ],
            invalid_recurrence_message("not a cron expression"),
        ),
        (
            "a recurrence whose hour is not an hour",
            group,
            vec![("Recurrence", "0 99 * * *"), ("DesiredCapacity", "1")],
            invalid_recurrence_message("0 99 * * *"),
        ),
        (
            "a recurrence whose day of the week is not a day",
            group,
            vec![("Recurrence", "0 0 * * 9"), ("DesiredCapacity", "1")],
            invalid_recurrence_message("0 0 * * 9"),
        ),
        (
            // The six-field `cron(…)` body other AWS services take is not this grammar.
            "a six-field AWS cron body",
            group,
            vec![("Recurrence", "0 10 * * ? *"), ("DesiredCapacity", "1")],
            invalid_recurrence_message("0 10 * * ? *"),
        ),
        (
            "a time zone the IANA database does not name",
            group,
            vec![
                ("Recurrence", "0 0 * * *"),
                ("TimeZone", "Mars/Olympus_Mons"),
                ("DesiredCapacity", "1"),
            ],
            INVALID_TIME_ZONE_MESSAGE.to_string(),
        ),
        (
            "a time zone named by its abbreviation rather than canonically",
            group,
            vec![
                ("Recurrence", "0 0 * * *"),
                ("TimeZone", "PST"),
                ("DesiredCapacity", "1"),
            ],
            INVALID_TIME_ZONE_MESSAGE.to_string(),
        ),
        (
            "a desired capacity above the bounds the action leaves the group with",
            group,
            vec![
                ("StartTime", "2030-06-01T12:00:00Z"),
                ("DesiredCapacity", "9"),
            ],
            desired_capacity_message(9, GROUP_MIN_SIZE, GROUP_MAX_SIZE),
        ),
        (
            "a desired capacity below the minimum the action itself sets",
            group,
            vec![
                ("StartTime", "2030-06-01T12:00:00Z"),
                ("MinSize", "2"),
                ("DesiredCapacity", "1"),
            ],
            desired_capacity_message(1, 2, GROUP_MAX_SIZE),
        ),
        (
            "a maximum below the minimum",
            group,
            vec![
                ("StartTime", "2030-06-01T12:00:00Z"),
                ("MinSize", "5"),
                ("MaxSize", "1"),
            ],
            max_below_min_message(1, 5),
        ),
        (
            "a negative minimum",
            group,
            vec![("StartTime", "2030-06-01T12:00:00Z"), ("MinSize", "-1")],
            "The MinSize must not be negative, but it is -1".to_string(),
        ),
        (
            "a group the account does not hold",
            absent,
            vec![
                ("StartTime", "2030-06-01T12:00:00Z"),
                ("DesiredCapacity", "1"),
            ],
            ABSENT_GROUP_WITHOUT_DETAIL.to_string(),
        ),
    ];

    for (case, named_group, definition, expected) in &rows {
        let (status, code, message) = put_action(&router, named_group, "refused", definition).await;
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

    // Nothing any refused row stated reached the store.
    let answer = describe(
        &router,
        "the group after every refusal",
        &[p("AutoScalingGroupName", group)],
    )
    .await;
    let names = reported_names(&answer);
    assert!(
        names.is_empty(),
        "expected a refused put to store nothing, got {names:?} in {answer}"
    );
}

/// RULE 5: the read selects by group, by name and by window, pages its answer, and refuses a
/// selection it cannot serve.
#[tokio::test]
async fn a_read_selects_by_group_name_and_window_and_pages_its_answer() {
    let first_group = "window-group-one";
    let second_group = "window-group-two";
    let router = given_group(first_group).await;
    create_group(&router, second_group).await;

    for (name, start_time) in [
        ("early", "2030-01-01T00:00:00Z"),
        ("middle", "2031-01-01T00:00:00Z"),
        ("late", "2032-01-01T00:00:00Z"),
    ] {
        put_served(
            &router,
            name,
            first_group,
            name,
            &[("StartTime", start_time), ("DesiredCapacity", "1")],
        )
        .await;
    }
    put_served(
        &router,
        "the other group's action",
        second_group,
        "other-group-action",
        &[
            ("StartTime", "2030-01-01T00:00:00Z"),
            ("DesiredCapacity", "1"),
        ],
    )
    .await;

    // (case, the selection the read carries, the action names it must report)
    let rows: Vec<SelectionCase<'_>> = vec![
        (
            "one group",
            vec![p("AutoScalingGroupName", first_group)],
            vec!["early", "late", "middle"],
        ),
        (
            "the account, with no group named",
            vec![],
            vec!["early", "late", "middle", "other-group-action"],
        ),
        (
            "two names, one of them unknown",
            vec![
                p("AutoScalingGroupName", first_group),
                p("ScheduledActionNames.member.1", "late"),
                p("ScheduledActionNames.member.2", "nonesuch"),
            ],
            vec!["late"],
        ),
        (
            "only a name nothing answers to",
            vec![
                p("AutoScalingGroupName", first_group),
                p("ScheduledActionNames.member.1", "nonesuch"),
            ],
            vec![],
        ),
        (
            "a window from the second year on",
            vec![
                p("AutoScalingGroupName", first_group),
                p("StartTime", "2031-01-01T00:00:00Z"),
            ],
            vec!["late", "middle"],
        ),
        (
            "a window up to the second year",
            vec![
                p("AutoScalingGroupName", first_group),
                p("EndTime", "2031-01-01T00:00:00Z"),
            ],
            vec!["early", "middle"],
        ),
        (
            "a window around the second year alone",
            vec![
                p("AutoScalingGroupName", first_group),
                p("StartTime", "2030-06-01T00:00:00Z"),
                p("EndTime", "2031-06-01T00:00:00Z"),
            ],
            vec!["middle"],
        ),
        (
            // "If scheduled action names are provided, this property is ignored."
            "a window the named action falls outside",
            vec![
                p("AutoScalingGroupName", first_group),
                p("ScheduledActionNames.member.1", "late"),
                p("StartTime", "2030-01-01T00:00:00Z"),
                p("EndTime", "2030-02-01T00:00:00Z"),
            ],
            vec!["late"],
        ),
    ];

    for (case, selection, expected) in &rows {
        let answer = describe(&router, case, selection).await;
        let names = reported_names(&answer);
        let expected: Vec<String> = expected.iter().map(|name| (*name).to_string()).collect();
        assert_eq!(
            names, expected,
            "expected the actions {expected:?} for {case:?}, got {names:?}"
        );
    }

    // One page at a time: a page carries the size asked for and a token, and the token
    // resumes where it left off.
    let page = describe(
        &router,
        "the first page of two",
        &[
            p("AutoScalingGroupName", first_group),
            p("MaxRecords", "2"),
        ],
    )
    .await;
    let names = reported_names(&page);
    assert_eq!(
        names,
        vec!["early".to_string(), "late".to_string()],
        "expected the first page to carry the first two actions, got {names:?}"
    );
    let token = tag_value(&page, "NextToken");
    assert!(
        !token.is_empty(),
        "expected a page that does not exhaust the list to carry a token, got {page}"
    );
    let next = describe(
        &router,
        "the second page of two",
        &[
            p("AutoScalingGroupName", first_group),
            p("MaxRecords", "2"),
            p("NextToken", token.clone()),
        ],
    )
    .await;
    let names = reported_names(&next);
    assert_eq!(
        names,
        vec!["middle".to_string()],
        "expected the second page to resume at the third action, got {names:?}"
    );
    assert!(
        tag_value(&next, "NextToken").is_empty(),
        "expected the last page to carry no token, got {next}"
    );

    // (case, the selection, the wire code and the whole message it is refused with)
    let refusals: Vec<ReadRefusalCase<'_>> = vec![
        (
            "a page size below the smallest the read serves",
            vec![p("MaxRecords", "0")],
            "ValidationError",
            "1 validation error detected: Value '0' at 'maxRecords' failed to satisfy \
             constraint: Member must have value greater than or equal to 1"
                .to_string(),
        ),
        (
            "a page size above the largest",
            vec![p("MaxRecords", "101")],
            "ValidationError",
            "1 validation error detected: Value '101' at 'maxRecords' failed to satisfy \
             constraint: Member must have value less than or equal to 100"
                .to_string(),
        ),
        (
            "a token this service never issued",
            vec![p("NextToken", "not-a-token")],
            "InvalidNextToken",
            "The token 'not-a-token' is invalid".to_string(),
        ),
        (
            "a group the account does not hold",
            vec![p("AutoScalingGroupName", "no-such-group")],
            "ValidationError",
            absent_group_message("no-such-group"),
        ),
    ];

    for (case, selection, expected_code, expected_message) in &refusals {
        let (status, code, message, body) = describe_result(&router, selection).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body}"
        );
        assert_eq!(
            (code.as_str(), message.as_str()),
            (*expected_code, expected_message.as_str()),
            "case {case:?}: expected {expected_code} {expected_message:?}, got {code} \
             {message:?}"
        );
    }
}

/// RULE 6: a scheduled action only exists against a group that exists.
#[tokio::test]
async fn deleting_the_group_takes_its_scheduled_actions_with_it() {
    let group = "deleted-group";
    let kept_group = "kept-group";
    let router = given_group(group).await;
    create_group(&router, kept_group).await;

    for (named_group, name) in [(group, "goes"), (kept_group, "stays")] {
        put_served(
            &router,
            name,
            named_group,
            name,
            &[
                ("StartTime", "2030-06-01T12:00:00Z"),
                ("DesiredCapacity", "1"),
            ],
        )
        .await;
    }

    let (status, body) = query(
        &router,
        "DeleteAutoScalingGroup",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "the delete must succeed, got {status} with {body:?}"
    );

    let answer = describe(&router, "the account after the delete", &[]).await;
    let names = reported_names(&answer);
    assert_eq!(
        names,
        vec!["stays".to_string()],
        "expected the deleted group's action to go with it, got {names:?} in {answer}"
    );
}

/// RULE 4, the other error class: a ceiling on the account's resources is the operation's
/// MODELLED `LimitExceededFault` (wire code `LimitExceeded`) rather than the `ValidationError`
/// every bad member value answers, and nothing is stored either way.
#[tokio::test]
async fn a_quota_is_answered_as_the_modelled_limit_exceeded_fault() {
    let group = "quota-group";
    let router = given_group(group).await;

    // A size past the ceiling this process holds a group to.
    let (status, code, message) = put_action(
        &router,
        group,
        "past-the-ceiling",
        &[
            ("StartTime", "2030-06-01T12:00:00Z"),
            ("MaxSize", &(MAX_GROUP_CAPACITY + 1).to_string()),
        ],
    )
    .await;
    assert_eq!(
        (status, code.as_str(), message.as_str()),
        (
            400,
            "LimitExceeded",
            format!(
                "The group size {} exceeds the limit of {MAX_GROUP_CAPACITY} instances per Auto \
                 Scaling group",
                MAX_GROUP_CAPACITY + 1
            )
            .as_str()
        ),
        "a size past the ceiling must answer the modelled fault, got {status} {code:?} \
         {message:?}"
    );

    // The group holds as many actions as the quota allows, and then one more.
    for index in 0..MAX_SCHEDULED_ACTIONS_PER_GROUP {
        put_served(
            &router,
            "filling the quota",
            group,
            &format!("action-{index:03}"),
            &[("StartTime", "2030-06-01T12:00:00Z"), ("MinSize", "1")],
        )
        .await;
    }
    let (status, code, message) = put_action(
        &router,
        group,
        "one-too-many",
        &[("StartTime", "2030-06-01T12:00:00Z"), ("MinSize", "1")],
    )
    .await;
    assert_eq!(
        (status, code.as_str(), message.as_str()),
        (
            400,
            "LimitExceeded",
            format!(
                "The maximum limit of {MAX_SCHEDULED_ACTIONS_PER_GROUP} scheduled actions has \
                 been reached."
            )
            .as_str()
        ),
        "one action past the quota must answer the modelled fault, got {status} {code:?} \
         {message:?}"
    );

    // The quota is a ceiling on CREATES: a put of a name the group already holds is still
    // served, and the refused one stored nothing.
    put_served(
        &router,
        "re-putting a held name at the quota",
        group,
        "action-000",
        &[("StartTime", "2031-06-01T12:00:00Z"), ("MinSize", "2")],
    )
    .await;
    // The whole collection, one page at a time: the read pages its answer, so counting the
    // group's actions means walking the pages the token links.
    let mut names: Vec<String> = Vec::new();
    let mut token = String::new();
    // Bounded: the quota is the most actions the group can hold, so at most that many pages
    // of one can exist.
    for _ in 0..=MAX_SCHEDULED_ACTIONS_PER_GROUP {
        let mut selection = vec![p("AutoScalingGroupName", group), p("MaxRecords", "100")];
        if !token.is_empty() {
            selection.push(p("NextToken", token.clone()));
        }
        let answer = describe(&router, "the group at its quota", &selection).await;
        names.extend(reported_names(&answer));
        token = tag_value(&answer, "NextToken");
        if token.is_empty() {
            break;
        }
    }
    assert_eq!(
        names.len(),
        MAX_SCHEDULED_ACTIONS_PER_GROUP,
        "expected the group to hold exactly the quota, got {} actions: {names:?}",
        names.len()
    );
    assert!(
        !names.contains(&"one-too-many".to_string())
            && !names.contains(&"past-the-ceiling".to_string()),
        "expected a refused put to store nothing, got {names:?}"
    );
}
