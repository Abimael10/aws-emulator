// SPDX-License-Identifier: CC0-1.0

//! Putting SEVERAL scheduled scaling actions of one Auto Scaling group in a single call, over
//! the awsQuery wire: what `BatchPutScheduledUpdateGroupAction` creates or updates, what it
//! reports about an action it could not, and what `DescribeScheduledActions` then reports
//! back.
//!
//! The batch writes the same resource `PutScheduledUpdateGroupAction` writes — the sibling
//! file beside this one pins the single put — so it is the parent suite's own concern, in its
//! own file.
//!
//! The rules under test:
//!
//! 1. One call puts EVERY action it states, each stated whole, and the read then reports all
//!    of them against the group the call named, carrying the members each action stated; a
//!    call that could store every action it stated reports no failed action at all.
//! 2. The batch is the same upsert as the single put, into the same collection: a call naming
//!    an action the group already holds REPLACES that definition and keeps the identity minted
//!    when the action was created — whichever of the two operations created it.
//! 3. An action the call could not store is reported on its own under
//!    `FailedScheduledUpdateGroupActions` ("the names of the scheduled actions that could not
//!    be created or updated, including an error message"), under the wire error code of its
//!    class, and the actions stated beside it are still written.
//! 4. Every action of the call belongs to the one group the call names, so a call naming a
//!    group the account does not hold is refused for the group with HTTP 400
//!    `ValidationError` and writes nothing.
//! 5. "One or more scheduled actions. The maximum number allowed is 50."
//!    (`BatchPutScheduledUpdateGroupActionType$ScheduledUpdateGroupActions`): a call stating
//!    that many is served whole and one stating more is refused for the member, writing
//!    nothing.
//!
//! Each table spans what its rule generalises over (how many actions one call states, how
//! each is timed, which of the group's sizes each sets, and which part of a definition cannot
//! be scheduled) rather than one recorded row, every expected sentence is written out here
//! rather than taken from the crate, and every assertion names its case and the value
//! actually reported.

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    borrowed, list_members, max_below_min_message, new_engine, p, query, router_with, tag_value,
    xml_unescape,
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

/// The most actions ONE call may state, from the member's own documentation: "One or more
/// scheduled actions. The maximum number allowed is 50." Written out here rather than taken
/// from the crate, so the assertion does not take the handler's own word for it.
const MAX_SCHEDULED_ACTIONS_PER_CALL: usize = 50;

/// The most instances this process holds one group to — the ceiling an action's size is held
/// to, which the operations answer their modelled `LimitExceededFault` for.
const MAX_GROUP_CAPACITY: i32 = 1_000;

/// The whole message an action stating none of the three sizes is refused with: the rule as
/// the member's own documentation states it.
const SIZE_REQUIRED_MESSAGE: &str =
    "You must specify at least one of the following properties: MaxSize, MinSize, or \
     DesiredCapacity";

/// The whole message an action whose window does not open is refused with.
const END_TIME_MESSAGE: &str = "Given end time must be after start time";

/// The whole message an action whose `Recurrence` is not the documented crontab expression is
/// refused with: the sentence this service states for that class, quoting the rejected value.
fn invalid_recurrence_message(recurrence: &str) -> String {
    format!("Given recurrence string: {recurrence} is invalid")
}

/// The whole message an action whose `TimeZone` is not an IANA name is refused with:
/// entirely fixed text, with no part of the request in it.
const INVALID_TIME_ZONE_MESSAGE: &str =
    "Time zone specified does not conform to standardized time zones found in IANA Time Zone \
     Database";

/// The whole message an action whose sizes the group could not hold is refused with: the
/// sentence every writer of a group's three sizes states.
fn desired_capacity_message(desired_capacity: i32, min_size: i32, max_size: i32) -> String {
    format!(
        "Desired capacity:{desired_capacity} must be between the specified min \
         size:{min_size} and max size:{max_size}"
    )
}

/// The whole message an action whose size is past the ceiling this process holds a group to is
/// refused with.
fn size_past_ceiling_message(size: i32) -> String {
    format!(
        "The group size {size} exceeds the limit of {MAX_GROUP_CAPACITY} instances per Auto \
         Scaling group"
    )
}

/// The whole message a call naming a group the account does not hold is refused with: the
/// absent-group prefix this service opens every such refusal with, and the detail form this
/// write serves — the detail field left unfilled, so it travels as the literal token `null`
/// and no part of the request appears in it. The same sentence the single put of a scheduled
/// action serves.
const ABSENT_GROUP_MESSAGE: &str = "AutoScalingGroup name not found - null";

/// The whole message a call stating more than [`MAX_SCHEDULED_ACTIONS_PER_CALL`] actions is
/// refused with: the sentence a list member past the bound it has to satisfy is refused with,
/// naming the member as the wire names it and the bound the call missed.
fn too_many_actions_message(count: usize) -> String {
    format!(
        "1 validation error detected: Value with {count} items at \
         'scheduledUpdateGroupActions' failed to satisfy constraint: Member must have length \
         less than or equal to {MAX_SCHEDULED_ACTIONS_PER_CALL}"
    )
}

/// One action a call states, as the borrowed pairs a row writes its members with.
type Definition<'a> = Vec<(&'a str, &'a str)>;

/// One action of a call: its name and the members it carries beyond that name.
type Action<'a> = (&'a str, Definition<'a>);

/// One call every action of which must be stored: the case, the actions it states, and the
/// members the read-back must then report for each of them.
type BatchCase<'a> = (&'a str, Vec<Action<'a>>, Vec<(&'a str, Definition<'a>)>);

/// One action a call cannot store: the case, the members it carries, and the wire error code
/// and whole message the answer must report FOR THAT ACTION.
type FailedCase<'a> = (&'a str, Definition<'a>, &'a str, String);

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
            ("MinSize", &GROUP_MIN_SIZE.to_string()),
            ("MaxSize", &GROUP_MAX_SIZE.to_string()),
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

/// Post one `BatchPutScheduledUpdateGroupAction` stating `actions` on `group`, and answer its
/// status, wire error code and message beside the body.
async fn batch_put(
    router: &MultiServiceRouter,
    group: &str,
    actions: &[Action<'_>],
) -> (u16, String, String, String) {
    let mut params = vec![p("AutoScalingGroupName", group)];
    for (index, (name, definition)) in actions.iter().enumerate() {
        let prefix = format!("ScheduledUpdateGroupActions.member.{}", index + 1);
        params.push(p(&format!("{prefix}.ScheduledActionName"), *name));
        for (member, value) in definition {
            params.push(p(&format!("{prefix}.{member}"), *value));
        }
    }
    let (status, body) = query(
        router,
        "BatchPutScheduledUpdateGroupAction",
        &borrowed(&params),
    )
    .await;
    (
        status,
        tag_value(&body, "Code"),
        xml_unescape(&tag_value(&body, "Message")),
        body,
    )
}

/// Post one `BatchPutScheduledUpdateGroupAction` that must be served, and answer the actions
/// it reported as failed.
async fn batch_served(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    actions: &[Action<'_>],
) -> Vec<(String, String, String)> {
    let (status, code, message, body) = batch_put(router, group, actions).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the call must be served, got {status} with {code:?} {message:?}"
    );
    failed_actions(&body)
}

/// Each action one answer reports as failed: its name, the wire error code and the whole
/// message, in the order the answer reports them.
fn failed_actions(xml: &str) -> Vec<(String, String, String)> {
    list_members(xml, "FailedScheduledUpdateGroupActions")
        .into_iter()
        .map(|block| {
            (
                tag_value(block, "ScheduledActionName"),
                tag_value(block, "ErrorCode"),
                xml_unescape(&tag_value(block, "ErrorMessage")),
            )
        })
        .collect()
}

/// Post one `DescribeScheduledActions` that must be served, and answer its body.
async fn describe(
    router: &MultiServiceRouter,
    case: &str,
    selection: &[(String, String)],
) -> String {
    let (status, body) = query(router, "DescribeScheduledActions", &borrowed(selection)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the read must succeed, got {status} with {body:?}"
    );
    body
}

/// Every scheduled action the group `group` holds, as its own `<member>` block, read in one
/// page large enough for every action a call of this suite can have stored.
async fn held_actions(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<String> {
    let answer = describe(
        router,
        case,
        &[p("AutoScalingGroupName", group), p("MaxRecords", "100")],
    )
    .await;
    assert!(
        tag_value(&answer, "NextToken").is_empty(),
        "case {case:?}: one page must hold every action of {group:?}, got {answer}"
    );
    list_members(&answer, "ScheduledUpdateGroupActions")
        .into_iter()
        .map(::std::string::ToString::to_string)
        .collect()
}

/// The names of the actions the group holds, in the order the read reports them.
async fn held_names(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<String> {
    held_actions(router, case, group)
        .await
        .iter()
        .map(|action| tag_value(action, "ScheduledActionName"))
        .collect()
}

/// The block of the action named `name` among the ones the group holds.
async fn held_action(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    name: &str,
) -> Option<String> {
    held_actions(router, case, group)
        .await
        .into_iter()
        .find(|action| tag_value(action, "ScheduledActionName") == name)
}

/// RULE 1: one call puts every action it states, each stated whole, and the read reports all
/// of them with the members each stated — and the call reports no failed action.
#[tokio::test]
async fn one_call_puts_every_action_it_states() {
    let rows: Vec<BatchCase<'_>> = vec![
        (
            "one action, timed by StartTime",
            vec![(
                "only",
                vec![("StartTime", "2030-06-01T12:00:00Z"), ("MinSize", "1")],
            )],
            vec![(
                "only",
                vec![
                    ("StartTime", "2030-06-01T12:00:00Z"),
                    ("Time", "2030-06-01T12:00:00Z"),
                    ("MinSize", "1"),
                ],
            )],
        ),
        (
            "two actions, one timed and one purely recurring",
            vec![
                (
                    "timed",
                    vec![
                        ("StartTime", "2031-01-02T03:04:05Z"),
                        ("DesiredCapacity", "2"),
                    ],
                ),
                (
                    "recurring",
                    vec![
                        ("Recurrence", "0 12 * * *"),
                        ("TimeZone", "Pacific/Tahiti"),
                        ("MinSize", "1"),
                        ("MaxSize", "3"),
                    ],
                ),
            ],
            vec![
                (
                    "timed",
                    vec![
                        ("StartTime", "2031-01-02T03:04:05Z"),
                        ("DesiredCapacity", "2"),
                    ],
                ),
                (
                    "recurring",
                    vec![
                        ("Recurrence", "0 12 * * *"),
                        ("TimeZone", "Pacific/Tahiti"),
                        ("MinSize", "1"),
                        ("MaxSize", "3"),
                    ],
                ),
            ],
        ),
        (
            "three actions, each setting a different one of the group's sizes",
            vec![
                (
                    "sets-min",
                    vec![("StartTime", "2032-03-04T05:06:07Z"), ("MinSize", "1")],
                ),
                (
                    "sets-max",
                    vec![("StartTime", "2032-03-05T05:06:07Z"), ("MaxSize", "3")],
                ),
                (
                    "sets-desired",
                    vec![
                        ("StartTime", "2032-03-06T05:06:07Z"),
                        ("EndTime", "2033-03-06T05:06:07Z"),
                        ("DesiredCapacity", "2"),
                    ],
                ),
            ],
            vec![
                ("sets-min", vec![("MinSize", "1")]),
                ("sets-max", vec![("MaxSize", "3")]),
                (
                    "sets-desired",
                    vec![
                        ("DesiredCapacity", "2"),
                        ("EndTime", "2033-03-06T05:06:07Z"),
                    ],
                ),
            ],
        ),
    ];

    for (case, actions, reported) in &rows {
        // One group per row, so the names the read reports are exactly the ones this call
        // stated.
        let group = format!("batch-{}", actions.len());
        let router = given_group(&group).await;

        let failed = batch_served(&router, case, &group, actions).await;
        assert!(
            failed.is_empty(),
            "case {case:?}: a call that could store every action must report none failed, \
             got {failed:?}"
        );

        let mut expected_names: Vec<String> =
            actions.iter().map(|(name, _)| (*name).to_string()).collect();
        // The collection is held action name by action name, which is the order every read
        // of it reports.
        expected_names.sort();
        let names = held_names(&router, case, &group).await;
        assert_eq!(
            names, expected_names,
            "case {case:?}: expected the group to hold {expected_names:?}, got {names:?}"
        );

        for (name, members) in reported {
            let action = held_action(&router, case, &group, name)
                .await
                .unwrap_or_else(|| panic!("case {case:?}: the read reports no action {name:?}"));
            let group_reported = tag_value(&action, "AutoScalingGroupName");
            assert_eq!(
                group_reported, group,
                "case {case:?}: expected {name:?} to report its group {group:?}, got \
                 {group_reported:?}"
            );
            // The ARN of a scheduled action names the account, the Region, this group and
            // this action.
            let arn = tag_value(&action, "ScheduledActionARN");
            assert!(
                arn.contains(&format!("autoScalingGroupName/{group}"))
                    && arn.ends_with(&format!("scheduledActionName/{name}")),
                "case {case:?}: expected {name:?} to report an ARN naming it, got {arn:?}"
            );
            for (member, expected) in members {
                let value = tag_value(&action, member);
                assert_eq!(
                    &value, expected,
                    "case {case:?}: expected {name:?} to report {member} {expected:?}, got \
                     {value:?} in {action}"
                );
            }
        }
    }
}

/// RULE 2: the batch upserts into the ONE collection both writers of a scheduled action
/// write: a call naming a held action replaces its definition and keeps the identity minted
/// when it was created, whichever operation created it.
#[tokio::test]
async fn a_batch_put_replaces_a_held_action_and_keeps_its_identity() {
    let group = "batch-upsert";
    let router = given_group(group).await;

    // One action created by the single put, one created by the batch: the update below
    // reaches both through the same call.
    let (status, code, message) = {
        let (status, body) = query(
            &router,
            "PutScheduledUpdateGroupAction",
            &[
                ("AutoScalingGroupName", group),
                ("ScheduledActionName", "from-single-put"),
                ("StartTime", "2030-06-01T12:00:00Z"),
                ("DesiredCapacity", "2"),
            ],
        )
        .await;
        (
            status,
            tag_value(&body, "Code"),
            xml_unescape(&tag_value(&body, "Message")),
        )
    };
    assert_eq!(
        status, 200,
        "the single put must succeed, got {status} with {code:?} {message:?}"
    );
    let failed = batch_served(
        &router,
        "creating the action the batch owns",
        group,
        &[(
            "from-batch-put",
            vec![("StartTime", "2030-07-01T12:00:00Z"), ("DesiredCapacity", "1")],
        )],
    )
    .await;
    assert!(
        failed.is_empty(),
        "the create must store its action, got {failed:?}"
    );

    let created: Vec<(String, String)> = {
        let mut created = Vec::new();
        for name in ["from-single-put", "from-batch-put"] {
            let action = held_action(&router, "the actions as created", group, name)
                .await
                .unwrap_or_else(|| panic!("the read reports no action {name:?}"));
            created.push((name.to_string(), tag_value(&action, "ScheduledActionARN")));
        }
        created
    };

    // One call restating both definitions WHOLE — a recurrence and a different size, with no
    // desired capacity at all — beside an action the group does not hold yet.
    let failed = batch_served(
        &router,
        "restating both definitions",
        group,
        &[
            (
                "from-single-put",
                vec![("Recurrence", "30 0 1 1,6,12 *"), ("MinSize", "1")],
            ),
            (
                "from-batch-put",
                vec![("StartTime", "2035-01-01T00:00:00Z"), ("MaxSize", "3")],
            ),
            (
                "new-in-the-update",
                vec![("StartTime", "2036-01-01T00:00:00Z"), ("MinSize", "2")],
            ),
        ],
    )
    .await;
    assert!(
        failed.is_empty(),
        "the update must store every action it restated, got {failed:?}"
    );

    let names = held_names(&router, "after the update", group).await;
    assert_eq!(
        names,
        vec![
            "from-batch-put".to_string(),
            "from-single-put".to_string(),
            "new-in-the-update".to_string()
        ],
        "expected the update to add one action and replace two, got {names:?}"
    );

    for (name, arn) in &created {
        let action = held_action(&router, "after the update", group, name)
            .await
            .unwrap_or_else(|| panic!("the read reports no action {name:?}"));
        let reported = tag_value(&action, "ScheduledActionARN");
        assert_eq!(
            &reported, arn,
            "expected {name:?} to keep the ARN minted when it was created ({arn:?}), got \
             {reported:?}"
        );
        assert!(
            tag_value(&action, "DesiredCapacity").is_empty(),
            "expected the update of {name:?} to replace its definition rather than merge into \
             it, got {action}"
        );
    }
    let replaced = held_action(&router, "after the update", group, "from-single-put")
        .await
        .expect("the read reports the replaced action");
    assert_eq!(
        (
            tag_value(&replaced, "Recurrence"),
            tag_value(&replaced, "MinSize")
        ),
        ("30 0 1 1,6,12 *".to_string(), "1".to_string()),
        "expected the update to state the action whole, got {replaced}"
    );
}

/// RULE 3: an action the call could not store is reported on its own, under the wire error
/// code of its class, and the actions stated beside it are still written.
#[tokio::test]
async fn an_action_that_cannot_be_scheduled_is_reported_on_its_own() {
    let rows: Vec<FailedCase<'_>> = vec![
        (
            "stating none of the three sizes",
            vec![("StartTime", "2030-06-01T12:00:00Z")],
            "ValidationError",
            SIZE_REQUIRED_MESSAGE.to_string(),
        ),
        (
            "bounds the group could not hold",
            vec![
                ("StartTime", "2030-06-01T12:00:00Z"),
                ("MinSize", "2"),
                ("MaxSize", "1"),
            ],
            "ValidationError",
            max_below_min_message(1, 2),
        ),
        (
            "a capacity outside the bounds it states",
            vec![
                ("StartTime", "2030-06-01T12:00:00Z"),
                ("MinSize", "1"),
                ("MaxSize", "2"),
                ("DesiredCapacity", "3"),
            ],
            "ValidationError",
            desired_capacity_message(3, 1, 2),
        ),
        (
            "a window that does not open",
            vec![
                ("StartTime", "2030-06-01T12:00:00Z"),
                ("EndTime", "2030-06-01T12:00:00Z"),
                ("MinSize", "1"),
            ],
            "ValidationError",
            END_TIME_MESSAGE.to_string(),
        ),
        (
            "a recurrence that is not a crontab expression",
            vec![("Recurrence", "every other tuesday"), ("MinSize", "1")],
            "ValidationError",
            invalid_recurrence_message("every other tuesday"),
        ),
        (
            "a time zone the IANA database does not name",
            vec![
                ("Recurrence", "0 12 * * *"),
                ("TimeZone", "Mars/Olympus_Mons"),
                ("MinSize", "1"),
            ],
            "ValidationError",
            INVALID_TIME_ZONE_MESSAGE.to_string(),
        ),
        (
            "a size past the ceiling a group is held to",
            vec![
                ("StartTime", "2030-06-01T12:00:00Z"),
                ("MaxSize", "1001"),
            ],
            "LimitExceeded",
            size_past_ceiling_message(MAX_GROUP_CAPACITY + 1),
        ),
    ];

    let group = "batch-partial";
    let router = given_group(group).await;

    let mut stored: Vec<String> = Vec::new();
    for (index, (case, definition, expected_code, expected_message)) in rows.iter().enumerate() {
        // The good action beside it is the control: a refused action must not take it down.
        let good = format!("served-{index:02}");
        let bad = format!("refused-{index:02}");
        let failed = batch_served(
            &router,
            case,
            group,
            &[
                (
                    good.as_str(),
                    vec![("StartTime", "2030-06-01T12:00:00Z"), ("MinSize", "1")],
                ),
                (bad.as_str(), definition.clone()),
            ],
        )
        .await;
        assert_eq!(
            failed,
            vec![(
                bad.clone(),
                (*expected_code).to_string(),
                expected_message.clone()
            )],
            "case {case:?}: expected the call to report only {bad:?} as failed, got {failed:?}"
        );
        stored.push(good);
    }

    stored.sort();
    let names = held_names(&router, "after every call", group).await;
    assert_eq!(
        names, stored,
        "expected the group to hold exactly the actions the calls could store, got {names:?}"
    );

    // Several actions of one call can fail, and each is reported: the answer carries one
    // entry per action, in the order the call stated them.
    let failed = batch_served(
        &router,
        "two actions the call cannot store",
        group,
        &[
            (
                "both-bad-1",
                vec![("StartTime", "2030-06-01T12:00:00Z"), ("MaxSize", "1001")],
            ),
            ("both-bad-2", vec![("StartTime", "2030-06-01T12:00:00Z")]),
        ],
    )
    .await;
    assert_eq!(
        failed,
        vec![
            (
                "both-bad-1".to_string(),
                "LimitExceeded".to_string(),
                size_past_ceiling_message(MAX_GROUP_CAPACITY + 1)
            ),
            (
                "both-bad-2".to_string(),
                "ValidationError".to_string(),
                SIZE_REQUIRED_MESSAGE.to_string()
            ),
        ],
        "expected one entry per action the call could not store, got {failed:?}"
    );
    let names = held_names(&router, "after the call that stored nothing", group).await;
    assert_eq!(
        names, stored,
        "expected a call that could store none of its actions to write nothing, got {names:?}"
    );
}

/// RULE 4: a call naming a group the account does not hold is refused for the group, and
/// writes nothing.
#[tokio::test]
async fn a_call_naming_no_stored_group_is_refused_for_the_group() {
    let group = "batch-held";
    let router = given_group(group).await;

    for absent in ["batch-absent", "", "batch-held-2"] {
        let (status, code, message, _) = batch_put(
            &router,
            absent,
            &[(
                "never-stored",
                vec![("StartTime", "2030-06-01T12:00:00Z"), ("MinSize", "1")],
            )],
        )
        .await;
        // An empty name states no group either, and is refused for the group the same way:
        // the modelled length bound makes it a boundary refusal, so only the status is the
        // same for it.
        if absent.is_empty() {
            assert_eq!(
                status, 400,
                "a call naming no group must be refused, got {status} with {code:?} {message:?}"
            );
            continue;
        }
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (400, "ValidationError", ABSENT_GROUP_MESSAGE),
            "a call against the absent group {absent:?} must be refused for the group, got \
             {status} {code:?} {message:?}"
        );
    }

    let names = held_names(&router, "the group the account does hold", group).await;
    assert!(
        names.is_empty(),
        "expected a refused call to write nothing, got {names:?}"
    );
}

/// RULE 5: a call may state at most 50 actions — that many is served whole, and one more is
/// refused for the member and writes nothing.
#[tokio::test]
async fn the_most_actions_one_call_may_state() {
    let group = "batch-bound";
    let router = given_group(group).await;

    // One past the bound FIRST, so what the group holds afterwards shows the refused call
    // wrote nothing.
    let over: Vec<String> = (0..=MAX_SCHEDULED_ACTIONS_PER_CALL)
        .map(|index| format!("over-{index:03}"))
        .collect();
    let over_actions: Vec<Action<'_>> = over
        .iter()
        .map(|name| {
            (
                name.as_str(),
                vec![("StartTime", "2030-06-01T12:00:00Z"), ("MinSize", "1")],
            )
        })
        .collect();
    let (status, code, message, _) = batch_put(&router, group, &over_actions).await;
    assert_eq!(
        (status, code.as_str(), message.as_str()),
        (
            400,
            "ValidationError",
            too_many_actions_message(MAX_SCHEDULED_ACTIONS_PER_CALL + 1).as_str()
        ),
        "a call stating {} actions must be refused for the member, got {status} {code:?} \
         {message:?}",
        over_actions.len()
    );
    let names = held_names(&router, "after the refused call", group).await;
    assert!(
        names.is_empty(),
        "expected the refused call to write nothing, got {names:?}"
    );

    // Exactly the bound: served whole, so the group holds every action the call stated.
    let at_bound: Vec<String> = (0..MAX_SCHEDULED_ACTIONS_PER_CALL)
        .map(|index| format!("bound-{index:03}"))
        .collect();
    let bound_actions: Vec<Action<'_>> = at_bound
        .iter()
        .map(|name| {
            (
                name.as_str(),
                vec![("StartTime", "2030-06-01T12:00:00Z"), ("MinSize", "1")],
            )
        })
        .collect();
    let failed = batch_served(&router, "at the bound", group, &bound_actions).await;
    assert!(
        failed.is_empty(),
        "a call at the bound must store every action it stated, got {failed:?}"
    );
    let names = held_names(&router, "at the bound", group).await;
    assert_eq!(
        names, at_bound,
        "expected the group to hold the {MAX_SCHEDULED_ACTIONS_PER_CALL} actions the call \
         stated, got {} of them: {names:?}",
        names.len()
    );
}
