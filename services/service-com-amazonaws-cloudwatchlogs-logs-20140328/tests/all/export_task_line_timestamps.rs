// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the TIMESTAMP each line of an export task's objects carries.
//!
//! CloudWatch Logs and Amazon S3 are mounted on one shared engine — the same assembly
//! `core/router` performs — so every object a case reads back is one the object store itself
//! holds, written by the export task over the in-process callback and not by anything here.
//!
//! RULE under test: a log event's timestamp rendered into exported log data carries a
//! FIXED-WIDTH millisecond fraction — the UTC calendar instant with exactly three
//! fractional-second digits, zero-padded — so the millisecond the event was stamped with is
//! what the line states, and a reader parsing every exported line with one millisecond format
//! reads them all. A fraction whose trailing zeros are trimmed states a different instant's
//! precision and breaks that reader; it also breaks the ordering a fixed-width rendering
//! gives, where comparing two rendered timestamps as text answers the same as comparing the
//! two instants.
//!
//! The table spans the fractions the rule generalises over: none at all, a single significant
//! digit, one and two trailing zeros, every digit significant, and the last millisecond of a
//! second.

use serde_json::json;

use crate::export_task_objects::Emulator;
use crate::harness::base_ms;

/// The destination prefix every export of this suite is pointed at.
const PREFIX: &str = "line-timestamps";

/// The window every export of this suite names, as milliseconds either side of the second the
/// events are planted in: wide enough to select every planted event.
const WINDOW_SLACK_MS: i64 = 60 * 1_000;

/// The millisecond fractions the table spans, each with the message of the event stamped with
/// it. A fraction is the event's own `timestamp` modulo one second, which is the value the
/// rendered line has to state in three digits.
const FRACTIONS: &[(&str, i64, &str)] = &[
    ("no fraction at all, on the whole second", 0, "whole-second"),
    ("a single significant digit", 7, "seven-ms"),
    ("one trailing zero", 630, "six-three-zero"),
    ("two trailing zeros", 600, "six-zero-zero"),
    ("a trailing zero in the middle digit", 109, "one-zero-nine"),
    ("every digit significant", 123, "one-two-three"),
    ("the last millisecond of the second", 999, "nine-nine-nine"),
];

/// The instant the planted events are stamped from: an in-window base
/// ([`crate::harness::base_ms`]) truncated to the whole second, so a case's own fraction is
/// the whole fraction of the event's timestamp.
fn base_second() -> i64 {
    let base = base_ms();
    base - base.rem_euclid(1_000)
}

/// The line an exported event must read, built from the event's own timestamp: the UTC
/// calendar instant to the second, then the event's millisecond component in exactly three
/// zero-padded digits, the UTC designator, a space and the message.
///
/// The fraction is written here by the test's own arithmetic rather than by a format string
/// borrowed from the emulator, so what is asserted is the rule and not the implementation.
fn expected_line(timestamp_ms: i64, message: &str) -> String {
    let instant = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(timestamp_ms)
        .unwrap_or_else(|| panic!("{timestamp_ms} is a calendar instant"));
    let millis = timestamp_ms.rem_euclid(1_000);
    format!(
        "{}.{millis:03}Z {message}",
        instant.format("%Y-%m-%dT%H:%M:%S")
    )
}

/// The timestamp field of one exported line — everything before the first space, which is
/// where the message begins.
fn stamp_of<'line>(case: &str, line: &'line str) -> &'line str {
    line.split_once(' ')
        .unwrap_or_else(|| panic!("case {case:?}: an exported line is a timestamp and a message, got {line:?}"))
        .0
}

/// Plant one stream of events at `base + offset` for each `(offset, message)`, export the
/// group into [`PREFIX`], and hand back the exported text once the task has settled.
///
/// The events are planted in the chronological order a `PutLogEvents` batch must be written
/// in, and the window is widened either side of them so the export's selection is never the
/// subject here.
async fn exported_lines(case: &str, group: &str, bucket: &str, events: &[(i64, &str)]) -> String {
    let emulator = Emulator::new();
    emulator.create_bucket(case, bucket).await;
    emulator
        .logs_ok(case, "CreateLogGroup", &json!({"logGroupName": group}))
        .await;
    emulator
        .logs_ok(
            case,
            "CreateLogStream",
            &json!({"logGroupName": group, "logStreamName": "stream"}),
        )
        .await;
    emulator
        .logs_ok(
            case,
            "PutLogEvents",
            &json!({
                "logGroupName": group,
                "logStreamName": "stream",
                "logEvents": crate::harness::batch(events),
            }),
        )
        .await;

    let first = events
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: the table plants at least one event"))
        .0;
    let last = events
        .last()
        .unwrap_or_else(|| panic!("case {case:?}: the table plants at least one event"))
        .0;
    let started = emulator
        .logs_ok(
            case,
            "CreateExportTask",
            &json!({
                "logGroupName": group,
                "from": first - WINDOW_SLACK_MS,
                "to": last + WINDOW_SLACK_MS,
                "destination": bucket,
                "destinationPrefix": PREFIX,
            }),
        )
        .await;
    let task_id = started["taskId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a started export reports a taskId, got {started}"))
        .to_string();

    let status = emulator.awaited_terminal_status(case, &task_id).await;
    assert_eq!(
        status, "COMPLETED",
        "case {case:?}: the export must complete, got {status}"
    );
    emulator.exported_text(case, bucket, PREFIX).await
}

/// Each exported line states its event's own millisecond in three zero-padded digits.
#[tokio::test]
async fn an_exported_line_renders_the_events_millisecond_in_three_digits() {
    let base = base_second();
    // One case per second, ascending, so the batch is chronological and the rendered
    // whole-second part varies across the table as well as the fraction.
    let planted: Vec<(i64, &str)> = FRACTIONS
        .iter()
        .enumerate()
        .map(|(index, (_, fraction, message))| {
            (base + i64::try_from(index).unwrap_or(0) * 1_000 + fraction, *message)
        })
        .collect();

    let text = exported_lines(
        "the millisecond fractions of the table",
        "export/line-timestamps",
        "export-line-timestamps",
        &planted,
    )
    .await;
    let lines: Vec<&str> = text.lines().collect();

    for (index, (case, _, message)) in FRACTIONS.iter().enumerate() {
        let (timestamp, _) = planted[index];
        let expected = expected_line(timestamp, message);
        let found = lines.iter().find(|line| line.ends_with(message));
        assert_eq!(
            found.copied(),
            Some(expected.as_str()),
            "case {case:?}: the exported line of the event stamped {timestamp} must read \
             {expected:?}, got {found:?} among {lines:?}"
        );
    }

    // Fixed width is what lets one reader parse every line: the timestamp field of each is
    // the same length, whatever the event's millisecond.
    let widths: Vec<usize> = lines
        .iter()
        .map(|line| stamp_of("the millisecond fractions of the table", line).len())
        .collect();
    assert!(
        widths.windows(2).all(|pair| pair[0] == pair[1]),
        "case {:?}: every exported timestamp has the same width, got {widths:?} for {lines:?}",
        "the millisecond fractions of the table"
    );
}

/// Two events of the SAME second render timestamps whose text order is their chronological
/// order — the invariant a fixed-width fraction gives and a trimmed one breaks.
#[tokio::test]
async fn exported_timestamps_of_one_second_sort_as_their_instants_do() {
    let case = "events a millisecond apart inside one second";
    let base = base_second();
    // Pairs that differ only in the trailing digit of the fraction: a rendering that trimmed
    // trailing zeros would sort the earlier event after the later one.
    let fractions: [i64; 6] = [0, 1, 600, 601, 630, 631];
    let messages: Vec<String> = fractions
        .iter()
        .map(|fraction| format!("ms-{fraction:03}"))
        .collect();
    let planted: Vec<(i64, &str)> = fractions
        .iter()
        .zip(messages.iter())
        .map(|(fraction, message)| (base + fraction, message.as_str()))
        .collect();

    let text = exported_lines(
        case,
        "export/line-timestamp-order",
        "export-line-timestamp-order",
        &planted,
    )
    .await;

    // The events were written oldest first, and the export writes them in that order, so the
    // rendered timestamps must already be ascending as text.
    let stamps: Vec<&str> = text.lines().map(|line| stamp_of(case, line)).collect();
    assert_eq!(
        stamps.len(),
        fractions.len(),
        "case {case:?}: every planted event is exported, got {text:?}"
    );
    for pair in stamps.windows(2) {
        assert!(
            pair[0] < pair[1],
            "case {case:?}: {:?} is the earlier instant, so it must sort before {:?}; got \
             {stamps:?}",
            pair[0],
            pair[1]
        );
    }
    // All six events sit in one second, so every line states the same whole second and
    // differs only in the fraction.
    let seconds: Vec<&str> = stamps
        .iter()
        .map(|stamp| stamp.split_once('.').map_or(*stamp, |(second, _)| second))
        .collect();
    assert!(
        seconds.windows(2).all(|pair| pair[0] == pair[1]),
        "case {case:?}: the events share one second, got {seconds:?}"
    );
    for (stamp, fraction) in stamps.iter().zip(fractions.iter()) {
        let rendered = stamp
            .strip_suffix('Z')
            .and_then(|stamp| stamp.split_once('.'))
            .map(|(_, fraction)| fraction.to_string())
            .unwrap_or_else(|| panic!("case {case:?}: {stamp:?} states a fraction before the Z"));
        assert_eq!(
            rendered,
            format!("{fraction:03}"),
            "case {case:?}: the event stamped {fraction} ms into the second renders that \
             millisecond in three digits, got {stamp:?}"
        );
    }
}
