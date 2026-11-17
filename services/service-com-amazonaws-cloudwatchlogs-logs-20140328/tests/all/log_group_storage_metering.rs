// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the figure `DescribeLogGroups` reports in
//! `logGroups[].storedBytes`.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.<Operation>` through the router that mounts it — so a
//! response that is not computed from the shared store cannot pass.
//!
//! The rule under test: `storedBytes` reports the volume the service's METERING last
//! measured for the log group, not the volume the group holds at the instant of the read —
//! so events ingested since the last pass are not counted, a group no pass has measured
//! reports zero however much has been written into it, and the figure moves only when the
//! next pass falls due. The tables below span that class along the axes the rule generalises
//! over (how many events and streams were written, how large the messages are, which
//! operation wrote them, the group's class, and the filter the listing selected the group
//! with); the case any one issue happens to mention is one row.
//!
//! No case waits for the metering cycle: the instant a group was last metered is pushed a
//! whole cycle into the past through the shared store ([`bring_metering_due`]) and the
//! figure is then read over the wire, so what each case asserts is what a client observes
//! once the pass the clock owed has been made.

use serde_json::{json, Value};

use crate::harness::{base_ms, batch, bring_metering_due, group_with_stream, Logs};

/// Per-event storage overhead the volume a log group stores counts. Source: the
/// `PutLogEvents` quota — a batch's size "is calculated as the sum of all event messages in
/// UTF-8, plus 26 bytes for each log event".
const EVENT_OVERHEAD_BYTES: i64 = 26;

/// Every log group class, from the model's `LogGroupClass` enum (`DELIVERY` |
/// `INFREQUENT_ACCESS` | `STANDARD`).
const LOG_GROUP_CLASSES: [&str; 3] = ["DELIVERY", "INFREQUENT_ACCESS", "STANDARD"];

/// What the events of `messages` add to the volume their log group holds.
fn held_bytes(messages: &[&str]) -> i64 {
    messages
        .iter()
        .map(|message| i64::try_from(message.len()).expect("a small message") + EVENT_OVERHEAD_BYTES)
        .sum()
}

/// The `storedBytes` `DescribeLogGroups` reports for `group`, selected by log group
/// identifier, with no metering pass brought due first.
async fn reported_bytes(logs: &Logs, group: &str) -> i64 {
    let page = logs
        .ok("DescribeLogGroups", &json!({"logGroupIdentifiers": [group]}))
        .await;
    page["logGroups"][0]
        .get("storedBytes")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| panic!("case {group}: a log group carries storedBytes, got {page}"))
}

/// How many events `GetLogEvents` reads back from one stream, so a case can tell a group
/// that was never metered from one that stored nothing.
async fn stored_events(logs: &Logs, group: &str, stream: &str) -> usize {
    let page = logs
        .ok(
            "GetLogEvents",
            &json!({"logGroupName": group, "logStreamName": stream, "startFromHead": true}),
        )
        .await;
    page.get("events")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {group}: GetLogEvents must return events, got {page}"))
        .len()
}

/// Write `messages` into `stream` of `group`, one event each, an increasing millisecond
/// apart inside the ingestion window.
async fn put(logs: &Logs, group: &str, stream: &str, messages: &[&str]) {
    let base = base_ms();
    let events: Vec<(i64, &str)> = messages
        .iter()
        .enumerate()
        .map(|(index, message)| (base + i64::try_from(index).expect("a small batch"), *message))
        .collect();
    logs.ok(
        "PutLogEvents",
        &json!({
            "logGroupName": group,
            "logStreamName": stream,
            "logEvents": batch(&events),
        }),
    )
    .await;
}

/// A freshly written log group reports no stored bytes, however much was written into it:
/// nothing has been metered yet. Over batches that differ in how many events they carry,
/// how large the messages are and how many streams they are spread over — and the events
/// are all readable, so the zero is the metering lag and not a lost write.
#[tokio::test]
async fn a_group_metering_has_not_measured_yet_reports_no_stored_bytes() {
    // A message far longer than the rest, so the table spans message SIZE as well as count.
    let kilobyte = "x".repeat(1_024);
    // (case, the messages written into each stream of the group)
    let cases: Vec<(&str, Vec<Vec<&str>>)> = vec![
        ("one small event", vec![vec!["hello"]]),
        ("a single-character message", vec![vec!["z"]]),
        (
            "a batch of five",
            vec![vec!["a", "bb", "ccc", "dddd", "eeeee"]],
        ),
        (
            "two streams",
            vec![vec!["first stream"], vec!["second stream", "and another"]],
        ),
        (
            "three streams, uneven",
            vec![
                vec!["one"],
                vec!["two", "three"],
                vec!["four", "five", "six"],
            ],
        ),
        ("a kilobyte of message", vec![vec![kilobyte.as_str()]]),
    ];

    for (case, streams) in &cases {
        let logs = Logs::new();
        let group = "metering-lag";
        logs.ok("CreateLogGroup", &json!({"logGroupName": group}))
            .await;
        let mut written: Vec<&str> = Vec::new();
        for (index, messages) in streams.iter().enumerate() {
            let stream = format!("stream-{index}");
            logs.ok(
                "CreateLogStream",
                &json!({"logGroupName": group, "logStreamName": stream}),
            )
            .await;
            put(&logs, group, &stream, messages).await;
            written.extend(messages.iter().copied());
            assert_eq!(
                stored_events(&logs, group, &stream).await,
                messages.len(),
                "case {case:?}: every event written into {stream} is stored"
            );
        }

        // Read twice: a listing does not move the figure either, so the second read reports
        // exactly what the first did.
        for read in ["the first read", "the second read"] {
            let reported = reported_bytes(&logs, group).await;
            assert_eq!(
                reported, 0,
                "case {case:?} / {read}: a group no metering pass has measured reports 0 \
                 stored bytes, got {reported}"
            );
        }

        // The volume IS there to be measured: the pass the clock owes reports it.
        bring_metering_due(&logs);
        let measured = reported_bytes(&logs, group).await;
        assert_eq!(
            measured,
            held_bytes(&written),
            "case {case:?}: once the pass has run, storedBytes is the volume the group \
             holds, got {measured}"
        );
    }
}

/// Each metering pass measures what the group holds when it falls due, and the figure stays
/// at that measurement until the next one: a write between two passes is counted by the
/// later one and by nothing before it. Over successive batches of different sizes.
#[tokio::test]
async fn stored_bytes_moves_only_when_a_metering_pass_falls_due() {
    let logs = Logs::new();
    let group = "metering-cycle";
    group_with_stream(&logs, group, "stream", None).await;

    // A message far longer than the rest, so the rounds span message SIZE too.
    let long = "y".repeat(512);
    // (case, the messages this round writes)
    let rounds: Vec<(&str, Vec<&str>)> = vec![
        ("the first batch", vec!["alpha"]),
        ("a wider batch", vec!["beta", "gamma", "delta"]),
        ("one long message", vec![long.as_str()]),
        ("a single-character message", vec!["z"]),
    ];

    let mut written: Vec<&str> = Vec::new();
    let mut measured_so_far = 0i64;
    for (case, messages) in &rounds {
        put(&logs, group, "stream", messages).await;
        let before = reported_bytes(&logs, group).await;
        assert_eq!(
            before, measured_so_far,
            "case {case:?}: a write does not move storedBytes before the next pass, got \
             {before}"
        );

        written.extend(messages.iter().copied());
        bring_metering_due(&logs);
        measured_so_far = held_bytes(&written);
        let after = reported_bytes(&logs, group).await;
        assert_eq!(
            after, measured_so_far,
            "case {case:?}: the pass measures everything the group now holds, got {after}"
        );
    }
}

/// The rule holds whatever the listing selected the group with, and whatever class the
/// group is in: the figure belongs to the group, not to the filter that found it.
#[tokio::test]
async fn the_metered_figure_is_the_same_whichever_filter_selects_the_group() {
    for class in LOG_GROUP_CLASSES {
        let logs = Logs::new();
        let group = format!("selection/{}", class.to_ascii_lowercase());
        logs.ok(
            "CreateLogGroup",
            &json!({"logGroupName": group, "logGroupClass": class}),
        )
        .await;
        logs.ok(
            "CreateLogStream",
            &json!({"logGroupName": group, "logStreamName": "stream"}),
        )
        .await;
        let messages = ["one", "two", "three"];
        put(&logs, &group, "stream", &messages).await;
        let (_arn, log_group_arn) = logs.group_arns(&group).await;

        // (case, the request that selects this group)
        let selections: Vec<(&str, Value)> = vec![
            ("no filter at all", json!({})),
            ("a name prefix", json!({"logGroupNamePrefix": group})),
            ("the group's name", json!({"logGroupIdentifiers": [group]})),
            (
                "the group's ARN",
                json!({"logGroupIdentifiers": [log_group_arn]}),
            ),
            ("its own class", json!({"logGroupClass": class})),
        ];

        for (selection, request) in &selections {
            let page = logs.ok("DescribeLogGroups", request).await;
            let reported = page["logGroups"][0]
                .get("storedBytes")
                .and_then(Value::as_i64);
            assert_eq!(
                reported,
                Some(0),
                "case class {class} selected by {selection}: a group no pass has measured \
                 reports 0, got {page}"
            );
        }

        bring_metering_due(&logs);
        for (selection, request) in &selections {
            let page = logs.ok("DescribeLogGroups", request).await;
            let reported = page["logGroups"][0]
                .get("storedBytes")
                .and_then(Value::as_i64);
            assert_eq!(
                reported,
                Some(held_bytes(&messages)),
                "case class {class} selected by {selection}: the measured figure is the \
                 volume held, got {page}"
            );
        }
    }
}

/// Each group is metered on its own: what one group holds is never reported for another,
/// and a group whose events have all gone is measured as empty by the next pass.
#[tokio::test]
async fn metering_measures_each_group_on_its_own() {
    let logs = Logs::new();
    // (group, the messages written into its one stream)
    let groups: Vec<(&str, Vec<&str>)> = vec![
        ("loud", vec!["a longer message", "and another one"]),
        ("quiet", vec!["x"]),
        ("silent", Vec::new()),
    ];

    for (group, messages) in &groups {
        group_with_stream(&logs, group, "stream", None).await;
        if !messages.is_empty() {
            put(&logs, group, "stream", messages).await;
        }
    }

    bring_metering_due(&logs);
    for (group, messages) in &groups {
        let reported = reported_bytes(&logs, group).await;
        assert_eq!(
            reported,
            held_bytes(messages),
            "case {group}: a group is measured by what IT holds, got {reported}"
        );
    }

    // The stream's events go with it, so the next pass measures the group as empty — and
    // until that pass, the figure is still the one the last measurement reported.
    logs.ok(
        "DeleteLogStream",
        &json!({"logGroupName": "loud", "logStreamName": "stream"}),
    )
    .await;
    let still = reported_bytes(&logs, "loud").await;
    assert_eq!(
        still,
        held_bytes(&groups[0].1),
        "case loud after the removal: storedBytes still reports the last measurement, got \
         {still}"
    );
    bring_metering_due(&logs);
    let remeasured = reported_bytes(&logs, "loud").await;
    assert_eq!(
        remeasured, 0,
        "case loud after the next pass: the removed stream's events are no longer measured, \
         got {remeasured}"
    );
    let untouched = reported_bytes(&logs, "quiet").await;
    assert_eq!(
        untouched,
        held_bytes(&groups[1].1),
        "case quiet: another group's removal measures nothing of this one, got {untouched}"
    );
}
