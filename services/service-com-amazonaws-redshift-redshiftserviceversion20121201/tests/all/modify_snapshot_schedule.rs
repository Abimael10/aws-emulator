// SPDX-License-Identifier: CC0-1.0

//! `ModifySnapshotSchedule`, end to end over the awsQuery wire.
//!
//! Every case drives a real HTTP request through the same `MultiServiceRouter`
//! the production router builds (the harness is shared with
//! [`crate::cluster_lifecycle`]), so each assertion covers the whole walk:
//! form-urlencoded deserialize -> handler -> XML serialize.
//!
//! The RULES these cases assert, each over a table of inputs rather than over
//! one recorded request:
//!
//! * the definitions a modify carries REPLACE the schedule's own, whatever
//!   expression type they are written in, how many there are and in which
//!   order, and they are what the schedule reports and what a later
//!   `DescribeSnapshotSchedules` reports; the members the request carries no
//!   value for — the description and the tags — are left as they were;
//! * a definition nothing can be computed from is refused with the one fault
//!   this operation declares for it, and the schedule keeps the definitions it
//!   had;
//! * only a schedule this account holds can be changed, so an identifier that
//!   names none is refused with the modelled not-found fault and writes
//!   nothing;
//! * a schedule a cluster is associated with carries the change for the span it
//!   takes to reach that cluster: while it does, the association is reported as
//!   still being applied and a second change is refused — and a schedule no
//!   cluster is associated with has nothing for the change to reach, so it
//!   takes one change after another.
//!
//! Nothing here waits on the wall clock: a case that has to observe both sides
//! of the span stops the emulator's clock and advances it, and the one wait for
//! a cluster to come up is a poll for that CONDITION through the shared
//! harness.

use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;

use crate::cluster_lifecycle::{all_text, Answer, Emulator};

/// One row of the replace table: what the schedule was created with and the
/// definitions the modify replaces them by.
struct ModifyCase {
    case: &'static str,
    /// The `ScheduleDefinitions` the create carried, in order.
    created: &'static [&'static str],
    /// The `ScheduleDefinitions` the modify carries, in order.
    modified: &'static [&'static str],
}

/// The replace table: one row per shape of change, differing along the
/// expression type the new definitions are written in, how many there are, and
/// the order they arrive in.
const MODIFY_CASES: &[ModifyCase] = &[
    ModifyCase {
        case: "a rate replaced by a rate of another period",
        created: &["rate(12 hours)"],
        modified: &["rate(6 hours)"],
    },
    ModifyCase {
        case: "a rate replaced by a calendar expression",
        created: &["rate(1 day)"],
        modified: &["cron(0 12 * * ? *)"],
    },
    ModifyCase {
        case: "a calendar expression replaced by a rate",
        created: &["cron(15 * * * ?)"],
        modified: &["rate(90 minutes)"],
    },
    ModifyCase {
        case: "one definition replaced by three",
        created: &["rate(12 hours)"],
        modified: &["rate(12 hours)", "cron(0 0 * * ? *)", "rate(100 minutes)"],
    },
    ModifyCase {
        case: "three definitions replaced by one",
        created: &["rate(12 hours)", "cron(0 0 * * ? *)", "rate(100 minutes)"],
        modified: &["cron(30 12 *)"],
    },
    ModifyCase {
        case: "the definitions reordered",
        created: &["rate(12 hours)", "cron(0 0 * * ? *)"],
        modified: &["cron(0 0 * * ? *)", "rate(12 hours)"],
    },
    ModifyCase {
        case: "the same definitions again",
        created: &["rate(12 hours)"],
        modified: &["rate(12 hours)"],
    },
];

/// The description and the tags every row of [`MODIFY_CASES`] is created with:
/// the modify's input carries neither member, so both have to survive it
/// unchanged.
const DESCRIPTION: &str = "the backups of this cluster";
/// See [`DESCRIPTION`].
const TAGS: &[(&str, &str)] = &[("owner", "admin"), ("environment", "production")];

/// The form parameters of a `CreateSnapshotSchedule` request.
fn create_params(
    identifier: &str,
    definitions: &[&str],
    description: Option<&str>,
    tags: &[(&str, &str)],
) -> Vec<(String, String)> {
    let mut params = vec![("ScheduleIdentifier".to_string(), identifier.to_string())];
    params.extend(definition_params(definitions));
    if let Some(description) = description {
        params.push(("ScheduleDescription".to_string(), description.to_string()));
    }
    for (position, (key, value)) in tags.iter().enumerate() {
        params.push((
            format!("Tags.Tag.{}.Key", position + 1),
            (*key).to_string(),
        ));
        params.push((
            format!("Tags.Tag.{}.Value", position + 1),
            (*value).to_string(),
        ));
    }
    params
}

/// The `ScheduleDefinitions` entries of a request, in the order they are given.
fn definition_params<S: AsRef<str>>(definitions: &[S]) -> Vec<(String, String)> {
    definitions
        .iter()
        .enumerate()
        .map(|(position, definition)| {
            (
                format!("ScheduleDefinitions.ScheduleDefinition.{}", position + 1),
                definition.as_ref().to_string(),
            )
        })
        .collect()
}

/// The form parameters of a `ModifySnapshotSchedule` request.
///
/// `pub(crate)` because the change path is driven from the snapshot-schedule
/// suite too (see [`crate::snapshot_schedules`]), so one encoding of
/// `ScheduleDefinitions` serves both rather than a copy per suite.
pub(crate) fn modify_params<S: AsRef<str>>(
    identifier: &str,
    definitions: &[S],
) -> Vec<(String, String)> {
    let mut params = vec![("ScheduleIdentifier".to_string(), identifier.to_string())];
    params.extend(definition_params(definitions));
    params
}

/// Drive one action with owned parameters, so a case can build its values.
async fn call_with(emulator: &Emulator, action: &str, params: &[(String, String)]) -> Answer {
    let borrowed: Vec<(&str, &str)> = params
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    emulator.call(action, &borrowed).await
}

/// `CreateSnapshotSchedule`, asserted to have minted the schedule.
async fn create_schedule(
    emulator: &Emulator,
    case: &str,
    identifier: &str,
    definitions: &[&str],
    description: Option<&str>,
    tags: &[(&str, &str)],
) {
    let created = call_with(
        emulator,
        "CreateSnapshotSchedule",
        &create_params(identifier, definitions, description, tags),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {case}: CreateSnapshotSchedule {identifier} expected 200, got {} with body {}",
        created.status, created.body
    );
}

/// `ModifySnapshotSchedule` for one identifier and one list of definitions.
async fn modify_schedule<S: AsRef<str>>(
    emulator: &Emulator,
    identifier: &str,
    definitions: &[S],
) -> Answer {
    call_with(
        emulator,
        "ModifySnapshotSchedule",
        &modify_params(identifier, definitions),
    )
    .await
}

/// `DescribeSnapshotSchedules` for one `ScheduleIdentifier`.
async fn describe_one(emulator: &Emulator, identifier: &str) -> Answer {
    emulator
        .call(
            "DescribeSnapshotSchedules",
            &[("ScheduleIdentifier", identifier)],
        )
        .await
}

/// The `<ScheduleDefinition>` entries a body carries, in document order.
fn listed_definitions(body: &str) -> Vec<String> {
    all_text(body, "ScheduleDefinition")
}

/// The tag key/value pairs a body carries, in document order.
fn listed_tags(body: &str) -> Vec<(String, String)> {
    all_text(body, "Key")
        .into_iter()
        .zip(all_text(body, "Value"))
        .collect()
}

/// The definitions the STORE holds for one schedule, read straight from this
/// service's state rather than from a response.
fn stored_definitions(emulator: &Emulator, identifier: &str) -> Option<Vec<String>> {
    let store = state::service_state(&*emulator.engine).expect("the Redshift store");
    let store = store.lock();
    store
        .snapshot_schedule(identifier)
        .map(|schedule| schedule.schedule_definitions.clone())
}

/// A table row's definitions as the store holds them.
fn owned(definitions: &[&str]) -> Vec<String> {
    definitions.iter().map(|value| (*value).to_string()).collect()
}

/// Seconds since the epoch now, for the "strictly in the future" bound on a
/// reported invocation.
fn now_seconds() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .expect("the clock is past the epoch")
            .as_secs(),
    )
    .expect("the clock is within the range a timestamp represents")
}

/// One reported invocation as seconds since the epoch.
fn invocation_seconds(case: &str, reported: &str) -> i64 {
    aws_smithy_types::DateTime::from_str(reported, aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_else(|error| {
            panic!("case {case}: expected an invocation time, got {reported:?} ({error})")
        })
        .secs()
}

/// Assert one refusal row: the answer's status, its `<Code>` and its `<Message>`.
fn assert_refused(case: &str, answer: &Answer, status: u16, code: &str, message: Option<&str>) {
    assert_eq!(
        answer.status, status,
        "case {case}: expected status {status}, got {} with body {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_code().as_deref(),
        Some(code),
        "case {case}: expected <Code>{code}</Code>, got {:?} with body {}",
        answer.error_code(),
        answer.body
    );
    if let Some(message) = message {
        assert_eq!(
            answer.text("Message").as_deref(),
            Some(message),
            "case {case}: expected <Message>{message}</Message>, got {:?} with body {}",
            answer.text("Message"),
            answer.body
        );
    }
}

/// The `ScheduleAssociationState` every association a body reports is in.
fn association_states(body: &str) -> Vec<String> {
    all_text(body, "ScheduleAssociationState")
}

/// The definitions a modify carries replace the schedule's own: the response
/// reports exactly them, a later read reports exactly them, the store holds
/// exactly them, and the members the request carries no value for are left as
/// the create set them.
#[tokio::test]
async fn the_definitions_a_modify_carries_replace_the_schedules_own() {
    let emulator = Emulator::new();
    for (index, row) in MODIFY_CASES.iter().enumerate() {
        let case = row.case;
        let identifier = format!("schedule-modify-{index:02}");
        create_schedule(
            &emulator,
            case,
            &identifier,
            row.created,
            Some(DESCRIPTION),
            TAGS,
        )
        .await;

        let before = now_seconds();
        let modified = modify_schedule(&emulator, &identifier, row.modified).await;
        assert_eq!(
            modified.status, 200,
            "case {case}: expected status 200, got {} with body {}",
            modified.status, modified.body
        );
        assert_eq!(
            modified.text("ScheduleIdentifier").as_deref(),
            Some(identifier.as_str()),
            "case {case}: expected the schedule it changed to be reported, got {:?}",
            modified.text("ScheduleIdentifier")
        );
        assert_eq!(
            listed_definitions(&modified.body),
            row.modified,
            "case {case}: expected the new definitions {:?}, got body {}",
            row.modified,
            modified.body
        );
        // The modify's input carries neither member, so the schedule keeps what
        // its create gave it.
        assert_eq!(
            modified.text("ScheduleDescription").as_deref(),
            Some(DESCRIPTION),
            "case {case}: expected the description to survive the change, got {:?}",
            modified.text("ScheduleDescription")
        );
        let expected_tags: Vec<(String, String)> = TAGS
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        assert_eq!(
            listed_tags(&modified.body),
            expected_tags,
            "case {case}: expected the tags to survive the change, got body {}",
            modified.body
        );
        // The schedule has no associated cluster, so the change had nothing to
        // reach and the response reports no association at all.
        assert_eq!(
            modified.text("AssociatedClusterCount").as_deref(),
            Some("0"),
            "case {case}: expected no associated cluster, got {:?} with body {}",
            modified.text("AssociatedClusterCount"),
            modified.body
        );
        // The invocations are computed from the NEW definitions, at the instant
        // of this call: as many as a read of a schedule reports, each still
        // ahead of the call.
        let invocations = all_text(&modified.body, "SnapshotTime");
        assert_eq!(
            invocations.len(),
            state::DEFAULT_REPORTED_NEXT_INVOCATIONS,
            "case {case}: expected {} upcoming invocation(s), got {invocations:?}",
            state::DEFAULT_REPORTED_NEXT_INVOCATIONS
        );
        for reported in &invocations {
            let at = invocation_seconds(case, reported);
            assert!(
                at > before,
                "case {case}: expected the invocation {reported} ({at}) to be ahead of the \
                 call ({before})"
            );
        }

        assert_eq!(
            stored_definitions(&emulator, &identifier).as_deref(),
            Some(&owned(row.modified)[..]),
            "case {case}: expected the store to hold the new definitions, got {:?}",
            stored_definitions(&emulator, &identifier)
        );
        let read_back = describe_one(&emulator, &identifier).await;
        assert_eq!(
            read_back.status, 200,
            "case {case}: DescribeSnapshotSchedules expected 200, got {} with body {}",
            read_back.status, read_back.body
        );
        assert_eq!(
            listed_definitions(&read_back.body),
            row.modified,
            "case {case}: expected the read to report the new definitions {:?}, got body {}",
            row.modified,
            read_back.body
        );
        assert_eq!(
            read_back.text("ScheduleDescription").as_deref(),
            Some(DESCRIPTION),
            "case {case}: expected the read to report the description, got {:?}",
            read_back.text("ScheduleDescription")
        );
    }
}

/// A definition nothing can be computed from is refused with the ONE fault this
/// operation declares for it — the create path's separate "unsupported type"
/// fault is not among this operation's errors — and the schedule keeps the
/// definitions it had.
#[tokio::test]
async fn a_definition_no_schedule_follows_from_leaves_the_schedule_as_it_was() {
    let emulator = Emulator::new();
    let identifier = "schedule-refused-change";
    let held = ["rate(12 hours)"];
    create_schedule(
        &emulator,
        "the schedule a refused change names",
        identifier,
        &held,
        None,
        &[],
    )
    .await;

    // The rows of the create path's own refusal table: the expression types this
    // service reads a schedule from are `rate` and `cron`, and an offered type
    // that states no schedule is refused too.
    let refused: Vec<(&str, Vec<String>)> = vec![
        ("a one-time at expression", vec!["at(2026-03-04T17:27:00)"]),
        ("an expression type that does not exist", vec!["every(12 hours)"]),
        ("an expression with no type", vec!["12 hours"]),
        ("an unclosed expression", vec!["rate(12 hours"]),
        ("a rate of zero", vec!["rate(0 hours)"]),
        ("a rate in a unit that is not one", vec!["rate(12 fortnights)"]),
        ("a rate with no unit", vec!["rate(12)"]),
        ("a negative rate value", vec!["rate(-12 hours)"]),
        ("a rate whose value is not a number", vec!["rate(twelve hours)"]),
        ("a cron expression of too few fields", vec!["cron(30 12)"]),
        ("a cron expression of too many fields", vec!["cron(0 0 12 * * ? *)"]),
        ("a cron field outside its range", vec!["cron(99 12 * * ? *)"]),
        (
            "one good definition and one bad one",
            vec!["rate(6 hours)", "rate(12 fortnights)"],
        ),
    ]
    .into_iter()
    .map(|(case, definitions)| {
        (
            case,
            definitions.into_iter().map(str::to_string).collect::<Vec<_>>(),
        )
    })
    .chain(std::iter::once((
        // One past the most definitions one schedule holds: the bound is the
        // store's own, and the change is refused on the count alone.
        "one definition more than a schedule holds",
        std::iter::repeat_n(
            "rate(12 hours)".to_string(),
            state::MAX_SCHEDULE_DEFINITIONS + 1,
        )
        .collect::<Vec<_>>(),
    )))
    .collect();

    for (case, definitions) in refused {
        let answer = modify_schedule(&emulator, identifier, &definitions).await;
        assert_refused(case, &answer, 400, "InvalidSchedule", None);
        assert_eq!(
            stored_definitions(&emulator, identifier).as_deref(),
            Some(&owned(&held)[..]),
            "case {case}: expected the schedule to keep {held:?}, got {:?}",
            stored_definitions(&emulator, identifier)
        );
    }
}

/// Only a schedule this account holds can be changed: an identifier that names
/// none is refused with the modelled not-found fault, in the wording the
/// service's other schedule-scoped operations use, and nothing is written.
#[tokio::test]
async fn a_schedule_this_account_does_not_hold_cannot_be_changed() {
    let emulator = Emulator::new();
    let held = "schedule-held";
    create_schedule(
        &emulator,
        "the one schedule this account holds",
        held,
        &["rate(12 hours)"],
        None,
        &[],
    )
    .await;

    for (case, named) in [
        ("a schedule nothing created", "no-such-schedule"),
        // The documented syntax states no case folding, so a schedule named in
        // another case is another schedule — and this account holds none.
        ("the held schedule in another case", "SCHEDULE-HELD"),
        ("an identifier one character longer", "schedule-held2"),
    ] {
        let answer = modify_schedule(&emulator, named, &["rate(6 hours)"]).await;
        assert_refused(
            case,
            &answer,
            400,
            "SnapshotScheduleNotFound",
            // The wording this operation words the fault with: the fixed text
            // "No snapshot schedule found for schedule identifier " and the
            // identifier the REQUEST spelled, bare.
            Some(&format!(
                "No snapshot schedule found for schedule identifier {named}"
            )),
        );
        assert_eq!(
            stored_definitions(&emulator, named),
            None,
            "case {case}: expected a refused change to mint no schedule, got {:?}",
            stored_definitions(&emulator, named)
        );
    }
    assert_eq!(
        stored_definitions(&emulator, held).as_deref(),
        Some(&["rate(12 hours)".to_string()][..]),
        "case the held schedule: expected the refused changes to leave it alone, got {:?}",
        stored_definitions(&emulator, held)
    );
}

/// A schedule no cluster is associated with has nothing for a change to reach,
/// so it takes one change after another: no window in which a second change is
/// refused, however close the two calls are.
#[tokio::test]
async fn a_schedule_no_cluster_is_associated_with_takes_a_change_at_once() {
    let emulator = Emulator::new();
    let identifier = "schedule-unassociated-changes";
    create_schedule(
        &emulator,
        "a schedule no cluster names",
        identifier,
        &["rate(12 hours)"],
        None,
        &[],
    )
    .await;
    // The clock stands still for both calls, so the span a change WOULD take if
    // the schedule had a cluster cannot pass between them: what this asserts is
    // that there is no such span at all.
    emulator.stop_clock();
    for (case, definitions) in [
        ("the first change", &["rate(6 hours)"]),
        ("the second change straight after it", &["rate(3 hours)"]),
    ] {
        let answer = modify_schedule(&emulator, identifier, definitions).await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected status 200, got {} with body {}",
            answer.status, answer.body
        );
        assert_eq!(
            stored_definitions(&emulator, identifier).as_deref(),
            Some(&definitions.map(str::to_string)[..]),
            "case {case}: expected the store to hold {definitions:?}, got {:?}",
            stored_definitions(&emulator, identifier)
        );
    }
}

/// A schedule a cluster is associated with is modified ASYNCHRONOUSLY: the
/// change is recorded by the call, the association is reported as still being
/// applied — on the schedule's read and on the cluster's own, which cannot
/// disagree — a second change is refused while it is, and once the span has
/// passed the association is active again and the next change is taken.
#[tokio::test]
async fn a_schedule_a_cluster_is_associated_with_takes_its_change_asynchronously() {
    let emulator = Emulator::new();
    let case = "a schedule a cluster is associated with";
    let identifier = "schedule-associated-changes";
    let cluster = "scheduled-cluster";

    let created = emulator.create_minimal(cluster).await;
    assert_eq!(
        created.status, 200,
        "case {case}: CreateCluster expected 200, got {} with body {}",
        created.status, created.body
    );
    emulator
        .poll_until(
            case,
            "DescribeClusters",
            &[("ClusterIdentifier", cluster)],
            |answer| answer.text("ClusterStatus").as_deref() == Some(state::STATUS_AVAILABLE),
        )
        .await;
    create_schedule(
        &emulator,
        case,
        identifier,
        &["rate(12 hours)"],
        None,
        &[],
    )
    .await;
    let associated = emulator
        .call(
            "ModifyClusterSnapshotSchedule",
            &[
                ("ClusterIdentifier", cluster),
                ("ScheduleIdentifier", identifier),
            ],
        )
        .await;
    assert_eq!(
        associated.status, 200,
        "case {case}: ModifyClusterSnapshotSchedule expected 200, got {} with body {}",
        associated.status, associated.body
    );

    // The span stays open for as long as this case needs, whatever the host is
    // doing: the clock the service measures it against stands still until the
    // advance below crosses it.
    emulator.stop_clock();
    let modified = modify_schedule(&emulator, identifier, &["rate(6 hours)"]).await;
    assert_eq!(
        modified.status, 200,
        "case {case}: expected status 200, got {} with body {}",
        modified.status, modified.body
    );
    assert_eq!(
        listed_definitions(&modified.body),
        ["rate(6 hours)"],
        "case {case}: expected the change to be recorded by the call, got body {}",
        modified.body
    );
    assert_eq!(
        association_states(&modified.body),
        [state::SCHEDULE_ASSOCIATION_STATE_MODIFYING.to_string()],
        "case {case}: expected the association to be reported as still being applied, got body {}",
        modified.body
    );
    let cluster_read = emulator
        .call("DescribeClusters", &[("ClusterIdentifier", cluster)])
        .await;
    assert_eq!(
        cluster_read.text("SnapshotScheduleState").as_deref(),
        Some(state::SCHEDULE_ASSOCIATION_STATE_MODIFYING),
        "case {case}: expected the cluster to report the same association state, got {:?}",
        cluster_read.text("SnapshotScheduleState")
    );

    // A change the service has not finished applying is what the modelled state
    // fault is for, and the refused change writes nothing.
    let refused = modify_schedule(&emulator, identifier, &["rate(3 hours)"]).await;
    assert_refused(
        "a second change while the first is being applied",
        &refused,
        400,
        "SnapshotScheduleUpdateInProgress",
        Some(&state::snapshot_schedule_update_in_progress_message(
            identifier,
        )),
    );
    assert_eq!(
        stored_definitions(&emulator, identifier).as_deref(),
        Some(&["rate(6 hours)".to_string()][..]),
        "case {case}: expected the refused change to write nothing, got {:?}",
        stored_definitions(&emulator, identifier)
    );

    // Once the span has passed the change has reached the cluster: the
    // association is active again on both reads, and the next change is taken.
    emulator.advance_clock(state::SNAPSHOT_SCHEDULE_UPDATE_SPAN);
    let settled = describe_one(&emulator, identifier).await;
    assert_eq!(
        association_states(&settled.body),
        [state::SCHEDULE_ASSOCIATION_STATE_ACTIVE.to_string()],
        "case {case}: expected the association to be active once the span passed, got body {}",
        settled.body
    );
    let cluster_settled = emulator
        .call("DescribeClusters", &[("ClusterIdentifier", cluster)])
        .await;
    assert_eq!(
        cluster_settled.text("SnapshotScheduleState").as_deref(),
        Some(state::SCHEDULE_ASSOCIATION_STATE_ACTIVE),
        "case {case}: expected the cluster to report the association as active, got {:?}",
        cluster_settled.text("SnapshotScheduleState")
    );
    let again = modify_schedule(&emulator, identifier, &["rate(3 hours)"]).await;
    assert_eq!(
        again.status, 200,
        "case {case}: expected the next change to be taken, got {} with body {}",
        again.status, again.body
    );
    assert_eq!(
        stored_definitions(&emulator, identifier).as_deref(),
        Some(&["rate(3 hours)".to_string()][..]),
        "case {case}: expected the store to hold the next change, got {:?}",
        stored_definitions(&emulator, identifier)
    );
}

/// A change is held to the SAME definition rule the create is: a snapshot
/// schedule takes at most one snapshot an hour, so a definition whose
/// invocations are closer together than [`state::MIN_SCHEDULE_INTERVAL`] is
/// refused with the modelled `InvalidSchedule`, naming the definition it was
/// given, and the schedule keeps the definitions it had.
///
/// The table spans the bound rather than one definition: the bound itself, one
/// step under it, the units a rate states an interval in, and the calendar
/// expressions whose named instants fall that close together.
#[tokio::test]
async fn a_change_to_a_definition_under_the_minimum_interval_is_refused() {
    let emulator = Emulator::new();
    let identifier = "schedule-interval-change";
    let held = ["rate(12 hours)"];
    create_schedule(
        &emulator,
        "the schedule a change under the bound names",
        identifier,
        &held,
        None,
        &[],
    )
    .await;

    // The bound is read off the service's own constant rather than spelled here,
    // so the row at it and the row under it follow the rule and not a number.
    let floor_minutes = state::MIN_SCHEDULE_INTERVAL.as_secs() / 60;
    let at_the_bound = format!("rate({floor_minutes} minutes)");
    let one_under = format!("rate({} minutes)", floor_minutes - 1);
    // Each row: the case, the definitions the change carries, and which of them
    // the refusal names — `None` for a change the service takes.
    let rows: Vec<(&str, Vec<String>, Option<usize>)> = vec![
        ("one minute under the bound", vec![one_under.clone()], Some(0)),
        ("a rate of a single minute", owned(&["rate(1 minute)"]), Some(0)),
        ("half the bound", owned(&["rate(30 minutes)"]), Some(0)),
        (
            "a calendar definition firing every minute",
            owned(&["cron(* * * * ?)"]),
            Some(0),
        ),
        (
            "a calendar definition firing twice an hour",
            owned(&["cron(0,30 * * * ?)"]),
            Some(0),
        ),
        (
            "a definition over the bound beside one under it",
            vec!["rate(12 hours)".to_string(), one_under.clone()],
            Some(1),
        ),
        ("the bound itself, stated in minutes", vec![at_the_bound.clone()], None),
        ("the bound stated in hours", owned(&["rate(1 hour)"]), None),
        (
            "a calendar definition firing once an hour",
            owned(&["cron(15 * * * ?)"]),
            None,
        ),
    ];

    for (case, definitions, offending) in &rows {
        let answer = modify_schedule(&emulator, identifier, definitions).await;
        match offending {
            Some(position) => {
                assert_refused(
                    case,
                    &answer,
                    400,
                    "InvalidSchedule",
                    Some(&format!(
                        "Schedule interval {} is less than minimum interval of {floor_minutes} \
                         minutes",
                        definitions[*position]
                    )),
                );
                assert_eq!(
                    stored_definitions(&emulator, identifier).as_deref(),
                    Some(&owned(&held)[..]),
                    "case {case}: expected the schedule to keep {held:?}, got {:?}",
                    stored_definitions(&emulator, identifier)
                );
            }
            None => {
                assert_eq!(
                    answer.status, 200,
                    "case {case}: expected {definitions:?} served with 200, got {} with body {}",
                    answer.status, answer.body
                );
                assert_eq!(
                    stored_definitions(&emulator, identifier).as_deref(),
                    Some(&definitions[..]),
                    "case {case}: expected {definitions:?} stored, got {:?}",
                    stored_definitions(&emulator, identifier)
                );
                // The next refused row has to find the schedule as it was left,
                // so the definitions it keeps are these from here on.
                let change_back = modify_schedule(&emulator, identifier, &owned(&held)).await;
                assert_eq!(
                    change_back.status, 200,
                    "case {case}: expected the schedule put back to {held:?}, got {} with body {}",
                    change_back.status, change_back.body
                );
            }
        }
    }
}
