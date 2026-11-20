// SPDX-License-Identifier: CC0-1.0

//! The SENTENCE an alarm explains a state with when the series it graded was published by
//! ANOTHER service's own work — here the `AWS/DynamoDB` `ConditionalCheckFailedRequests` series
//! a table's refused conditional writes publish — asserted end to end over the wire, with
//! CloudWatch, Amazon DynamoDB, SNS and SQS on one engine.
//!
//! The RULE asserted here: a metric alarm explains an evaluated state with the datapoints it
//! GRADED, whoever published them — `Threshold Crossed: <n> out of the last <m> datapoints
//! [<value> (<instant>), …] was|were [not] <comparison> the threshold (<threshold>) (minimum <k>
//! datapoint(s) for <from> -> <to> transition).` — so the value the sentence names is the
//! statistic the alarm's own period took over the series, the comparison and the threshold are
//! the ones the alarm is configured with, and the same sentence is the one its action carries to
//! the subscribers of the topic it names (`NewStateReason`). An alarm that reports a breach it
//! cannot say which datapoints caused — a state explained by the missing-data arm, or by a
//! count of datapoints it never received — is not grading the series the other service
//! published.
//!
//! The rows differ along what the rule generalises over: which write surface the refusals came
//! in through, how many writes were refused (so which value the sentence names), the comparison
//! operator the sentence words, and whether the threshold it cites is whole, zero or
//! fractional. Every row's expected sentence is computed from the row — the counts, the
//! comparison and the threshold it configures — rather than taken from a recording, and the one
//! part no row can know, the instant each graded datapoint was measured over, is checked to BE a
//! calendar instant instead of being compared against one rendering.
//!
//! Every wait here is for a CONDITION — the table a create owes, the state an evaluation owes,
//! the notification a transition owes, and the room in the period a row's refusals need — looked
//! at again until it holds or a generous ceiling passes, so how fast the host runs decides
//! nothing asserted here.

use std::time::Instant;

use serde_json::{json, Value};

use crate::dynamodb_conditional_write_alarm_notification::{
    create_table, dynamodb, dynamodb_ok, emulator_with_dynamodb_and_notifications, seeded_item,
    table_when_active, Surface, CONDITIONAL_CHECK_FAILED_REQUESTS, NAMESPACE, TABLE_DIMENSION,
};
use crate::harness::{
    error_code, given_topic_with_subscriber, rpc, until_notified, wait_for_period_room, Emulator,
    LOOK_INTERVAL, TARGET_PREFIX, WORK_WAIT,
};

/// The state every row's alarm is taken into by the refusals it grades.
const ALARM: &str = "ALARM";

/// The missing-data arm every row's alarm carries, as `TreatMissingData` spells it: a period
/// holding no datapoint is within the threshold, so a breach reported here can only have been
/// decided by a datapoint the table really published.
const NOT_BREACHING: &str = "notBreaching";

/// The alarm's period and how many of them it grades.
///
/// The series DynamoDB publishes is stored at standard resolution, which holds at most one
/// datapoint a minute, so one minute is the finest period it can be alarmed at ("Be sure to
/// specify 10, 20, or 30 only for metrics that are stored by a PutMetricData call with a
/// StorageResolution of 1", `smithy_query(query="doc",shape="PutMetricAlarmInput",
/// member="Period")`), and the one-period look-back is what a client writes for "any refusal
/// now".
const PERIOD_SECS: i32 = 60;
const EVALUATION_PERIODS: i32 = 1;

/// How much of the period a row's refusals need left when they are issued: the whole batch of
/// them is issued in one go, in process, so this is room for a publish rather than for a wait —
/// what it buys is that every refusal of a row lands in the SAME period, and so that the value
/// the graded period carries is the row's whole count.
const ROOM_FOR_THE_REFUSALS: f64 = 30.0;

/// One row: which surface refuses, how many writes it refuses, the threshold and comparison
/// their count is graded against, and the whole sentence the graded state must be explained
/// with.
struct Row {
    /// What this row is about, named by every assertion it makes.
    case: &'static str,
    /// The write surface its refusals come in through.
    surface: Surface,
    /// How many writes are turned down — and so the `Sum` the graded period carries.
    refusals: usize,
    /// The alarm's `Threshold`.
    threshold: f64,
    /// The alarm's `ComparisonOperator`.
    comparison: &'static str,
    /// The whole `StateReason` the evaluation must report, with `{instants}` standing for the
    /// bracketed datapoint list the row cannot know the instants of.
    reason: &'static str,
    /// The value that list must name as graded: the refusals of the period, as an explanation
    /// writes a measured quantity.
    graded: &'static str,
}

/// The table: one row per shape the rule generalises over.
const ROWS: &[Row] = &[
    Row {
        case: "twelve refused puts, at a threshold of ten",
        surface: Surface::Put,
        refusals: 12,
        threshold: 10.0,
        comparison: "GreaterThanOrEqualToThreshold",
        reason: "Threshold Crossed: 1 out of the last 1 datapoints {instants} was greater than \
                 or equal to the threshold (10.0) (minimum 1 datapoint for OK -> ALARM \
                 transition).",
        graded: "12.0",
    },
    Row {
        case: "one refused update, over a threshold of none",
        surface: Surface::Update,
        refusals: 1,
        threshold: 0.0,
        comparison: "GreaterThanThreshold",
        reason: "Threshold Crossed: 1 out of the last 1 datapoints {instants} was greater than \
                 the threshold (0.0) (minimum 1 datapoint for OK -> ALARM transition).",
        graded: "1.0",
    },
    Row {
        case: "four refused deletes, under a threshold of five",
        surface: Surface::Delete,
        refusals: 4,
        threshold: 5.0,
        comparison: "LessThanThreshold",
        reason: "Threshold Crossed: 1 out of the last 1 datapoints {instants} was less than the \
                 threshold (5.0) (minimum 1 datapoint for OK -> ALARM transition).",
        graded: "4.0",
    },
    Row {
        case: "two refused statements, at a threshold of two",
        surface: Surface::Statement,
        refusals: 2,
        threshold: 2.0,
        comparison: "LessThanOrEqualToThreshold",
        reason: "Threshold Crossed: 1 out of the last 1 datapoints {instants} was less than or \
                 equal to the threshold (2.0) (minimum 1 datapoint for OK -> ALARM \
                 transition).",
        graded: "2.0",
    },
    Row {
        case: "three refused transactional writes, over a fractional threshold",
        surface: Surface::Transaction,
        refusals: 3,
        threshold: 2.5,
        comparison: "GreaterThanThreshold",
        reason: "Threshold Crossed: 1 out of the last 1 datapoints {instants} was greater than \
                 the threshold (2.5) (minimum 1 datapoint for OK -> ALARM transition).",
        graded: "3.0",
    },
];

/// RULE: the state an alarm computes over a series another service published is explained by the
/// datapoints it graded — the statistic the period carried, the comparison it was read with and
/// the threshold it was compared against — and that same sentence reaches the subscribers of the
/// topic its action names.
#[tokio::test]
async fn the_reason_of_an_alarm_over_refused_writes_names_the_refusals_it_graded() {
    let emulator = emulator_with_dynamodb_and_notifications();

    // Every table is created before any of them is waited on, so the whole catalog becomes
    // ACTIVE over one span rather than one span each.
    let tables: Vec<String> = (0..ROWS.len())
        .map(|index| format!("ccf-reason-{index}"))
        .collect();
    for table in &tables {
        create_table(&emulator, table).await;
    }
    for (row, table) in ROWS.iter().zip(&tables) {
        table_when_active(&emulator, row.case, table).await;
        // The key the guarded writes address holds an item, so a refusal is a lost race rather
        // than a missing item. It applies, so it publishes nothing on the series under test.
        dynamodb_ok(
            &emulator,
            row.case,
            "PutItem",
            json!({"TableName": table, "Item": seeded_item()}),
        )
        .await;
    }

    let mut watched: Vec<(&Row, &String, String, String)> = Vec::new();
    for (index, (row, table)) in ROWS.iter().zip(&tables).enumerate() {
        let (topic, queue_url) =
            given_topic_with_subscriber(&emulator, &format!("ccf-reason-notify-{index}")).await;
        let alarm = format!("ccf-reason-alarm-{index}");
        given_alarm(&emulator, &alarm, table, row, &topic).await;
        watched.push((row, table, alarm, queue_url));
    }

    // The refusals of every row are issued together, with most of the period still ahead of
    // them: the period a datapoint belongs to is fixed by its own instant, so a row's whole
    // count is the value ONE period carries and the sentence that period is explained with
    // names it.
    wait_for_period_room(f64::from(PERIOD_SECS), ROOM_FOR_THE_REFUSALS).await;
    for (row, table, _, _) in &watched {
        let case = row.case;
        for attempt in 0..row.refusals {
            let (status, answered) = dynamodb(
                &emulator,
                row.surface.operation(),
                row.surface.refused_request(table),
            )
            .await;
            assert_eq!(
                (status, error_code(&answered)),
                (400, row.surface.refusal_code().to_string()),
                "case {case:?}, attempt {attempt}: expected the guarded {} to be turned down, \
                 got {status} ({answered})",
                row.surface.operation()
            );
        }
    }

    for (row, table, alarm, queue_url) in watched {
        let case = row.case;
        let reported = until_reported_state(&emulator, &alarm, ALARM).await;
        assert_eq!(
            reported["StateValue"].as_str(),
            Some(ALARM),
            "case {case:?}: {} refusal(s) against a threshold of {} read {} must take the alarm \
             to {ALARM}, it reports {reported}",
            row.refusals,
            row.threshold,
            row.comparison
        );

        let reason = reported["StateReason"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let (sentence, listed) = split_reason(&reason).unwrap_or_else(|| {
            panic!("case {case:?}: no bracketed datapoint list in StateReason {reason:?}")
        });
        assert_eq!(
            sentence, row.reason,
            "case {case:?}: expected the graded state of the {CONDITIONAL_CHECK_FAILED_REQUESTS} \
             series of {table:?} to be explained as {:?}, got {reason:?}",
            row.reason
        );

        // The one part the row cannot state: each graded datapoint is the value the period
        // carried, with the instant of the period it was measured over.
        let (value, instant) = listed.split_once(" (").unwrap_or((listed.as_str(), ""));
        assert_eq!(
            value, row.graded,
            "case {case:?}: expected the reason to name the {} refusal(s) the period carried as \
             {:?}, it names {listed:?}",
            row.refusals, row.graded
        );
        let instant = instant.strip_suffix(')').unwrap_or(instant);
        assert!(
            is_calendar_instant(instant),
            "case {case:?}: expected the graded datapoint to carry the instant of its period, \
             got {listed:?}"
        );

        // The subscribers of the topic the action names are told the same thing: the sentence
        // the state is explained with travels with the notification.
        let delivered = until_notified(&emulator, &queue_url).await;
        assert_eq!(
            delivered.len(),
            1,
            "case {case:?}: the transition must deliver exactly one notification to the \
             subscribed queue, got {delivered:?}"
        );
        let notification = &delivered[0];
        assert_eq!(
            notification["NewStateReason"].as_str(),
            Some(reason.as_str()),
            "case {case:?}: the notification must carry the sentence the state is explained \
             with, got {notification}"
        );
        assert_eq!(
            (
                notification["NewStateValue"].as_str(),
                notification["Trigger"]["Namespace"].as_str(),
                notification["Trigger"]["MetricName"].as_str(),
            ),
            (Some(ALARM), Some(NAMESPACE), Some(CONDITIONAL_CHECK_FAILED_REQUESTS)),
            "case {case:?}: the notification must carry the state moved to and the series it \
             fired on, got {notification}"
        );
    }
}

/// A metric alarm over the refusal series of `table`, configured as `row` states it, with
/// `topic` as its one alarm action.
async fn given_alarm(emulator: &Emulator, name: &str, table: &str, row: &Row, topic: &str) {
    rpc(
        emulator,
        "monitoring",
        TARGET_PREFIX,
        "PutMetricAlarm",
        &json!({
            "AlarmName": name,
            "Namespace": NAMESPACE,
            "MetricName": CONDITIONAL_CHECK_FAILED_REQUESTS,
            "Dimensions": [{"Name": TABLE_DIMENSION, "Value": table}],
            "Statistic": "Sum",
            "Period": PERIOD_SECS,
            "EvaluationPeriods": EVALUATION_PERIODS,
            "Threshold": row.threshold,
            "ComparisonOperator": row.comparison,
            "TreatMissingData": NOT_BREACHING,
            "AlarmActions": [topic],
        }),
    )
    .await;
}

/// Look at `DescribeAlarms` until the alarm `name` reports `wanted`, and answer the alarm
/// document that read reported.
///
/// The state and the sentence explaining it come out of the SAME read, so what is asserted
/// about the explanation is the explanation of the state that was asserted — a later evaluation
/// moving the alarm on cannot slip between the two.
async fn until_reported_state(emulator: &Emulator, name: &str, wanted: &str) -> Value {
    let deadline = Instant::now() + WORK_WAIT;
    loop {
        let described = rpc(
            emulator,
            "monitoring",
            TARGET_PREFIX,
            "DescribeAlarms",
            &json!({"AlarmNames": [name]}),
        )
        .await;
        let reported = described["MetricAlarms"][0].clone();
        if reported["StateValue"] == json!(wanted) || Instant::now() >= deadline {
            return reported;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The bracketed datapoint list of a reason, and the sentence with that list replaced by
/// `{instants}` — so a row can state the whole sentence without knowing the instants.
fn split_reason(reason: &str) -> Option<(String, String)> {
    let open = reason.find('[')?;
    let close = reason[open..].find(']')? + open;
    let listed = reason[open + 1..close].to_string();
    let sentence = format!("{}{{instants}}{}", &reason[..open], &reason[close + 1..]);
    Some((sentence, listed))
}

/// Whether `instant` is a calendar instant rather than a count or a placeholder: a date and a
/// time of day, made only of digits and the separators a calendar form uses.
///
/// Deliberately form-agnostic — the rule is that a graded datapoint carries the instant of the
/// period it was measured over, not that the instant is written one particular way.
fn is_calendar_instant(instant: &str) -> bool {
    /// The shortest calendar instant any form writes (`1/1/70 0:00:00`).
    const MIN_INSTANT_LEN: usize = 14;

    instant.len() >= MIN_INSTANT_LEN
        && instant.contains(':')
        && instant
            .chars()
            .filter(|character| character.is_ascii_digit())
            .count()
            >= 10
        && instant
            .chars()
            .all(|character| character.is_ascii_digit() || "-:/.TZ+ ".contains(character))
        // A refusal this suite made was counted seconds ago, so an instant at the start of the
        // epoch is a value nothing measured.
        && !instant.starts_with("1970")
        && !instant.contains("/70 ")
}
