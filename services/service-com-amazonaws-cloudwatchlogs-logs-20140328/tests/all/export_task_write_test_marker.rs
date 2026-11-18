// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the WRITE CHECK an export task leaves in its destination bucket.
//!
//! CloudWatch Logs and Amazon S3 are mounted on ONE shared engine — the same assembly
//! `core/router` performs — so every object a case reads back is one the object store itself
//! holds, written by the export task over the in-process callback and not by anything here.
//!
//! The rule under test: `CreateExportTask` requires "credentials that have permission to write
//! to the S3 bucket that you specify as the destination", and the export establishes that it
//! has them by WRITING — one small object of its own, named `aws-logs-write-test`, under the
//! task's `destinationPrefix` ("the prefix used as the start of the key for every object
//! exported"), before any exported log data. So by the time a task reports a terminal status,
//! its destination prefix carries that object, whatever the task's window selected: a prefix
//! an export ran into is never empty, and a client reading the prefix can tell the check apart
//! from the gzip-framed log data beside it.
//!
//! The table spans what the rule generalises over: what prefix the destination names (flat,
//! nested, one already ending in a separator, and none at all, which takes the documented
//! default), how many events the window selects (every one, some, none at all), and whether a
//! `logStreamNamePrefix` narrows the export to no stream.
//!
//! Every case waits for the terminal status the way a client does — poll
//! `DescribeExportTasks` until it is terminal — and then reads the bucket, so nothing here
//! waits for a fixed span of the clock.

use serde_json::{json, Value};

use crate::export_task_objects::{Emulator, WRITE_TEST_BODY, WRITE_TEST_OBJECT};
use crate::harness::now_ms;

/// Milliseconds in one hour.
const HOUR: i64 = 60 * 60 * 1_000;

/// The `destinationPrefix` an export task gets when the create names none — "if you don't
/// specify a value, the default is exportedlogs" (`CreateExportTask$destinationPrefix`).
const DEFAULT_PREFIX: &str = "exportedlogs";

/// The first two bytes of a gzip frame, which the write check is NOT one of: it is plain
/// text, unlike the exported log data beside it.
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// One row of the table.
struct Row {
    /// What the row is about, named in every assertion it makes.
    case: &'static str,
    /// The `destinationPrefix` the create names, or `None` to take the documented default.
    prefix: Option<&'static str>,
    /// The `logStreamNamePrefix` the create names, when it narrows the streams.
    stream_prefix: Option<&'static str>,
    /// The window, as whole hours before the clock: `[clock - from_hours, clock - to_hours)`.
    window: (i64, i64),
    /// How many gzip objects of exported log data the window then selects — one per stream
    /// that has an event in it.
    exported_objects: usize,
}

/// The events every row plants, as `(stream, whole hours before the clock, message)`.
const PLANTED: &[(&str, i64, &str)] = &[
    ("app/one", 3, "FIRST"),
    ("app/one", 2, "SECOND"),
    ("sys/two", 2, "OTHER-STREAM"),
];

/// The group every row exports, and the bucket it exports into.
const GROUP: &str = "export/write-check";
const BUCKET: &str = "export-write-check";

/// An export leaves its write check under its destination prefix by the time it reports a
/// terminal status, whatever its window selected.
#[tokio::test]
async fn an_export_leaves_its_write_check_under_the_destination_prefix() {
    let rows: [Row; 5] = [
        Row {
            case: "a flat prefix, with the window over every event",
            prefix: Some("checked"),
            stream_prefix: None,
            window: (4, 0),
            exported_objects: 2,
        },
        Row {
            case: "a nested prefix, with the window over one stream's events only",
            prefix: Some("deep/prefix/segments"),
            stream_prefix: None,
            window: (4, 2),
            exported_objects: 1,
        },
        Row {
            case: "a prefix that already ends in a separator",
            prefix: Some("trailing/"),
            stream_prefix: None,
            window: (4, 0),
            exported_objects: 2,
        },
        Row {
            case: "no prefix named, so the documented default is used",
            prefix: None,
            stream_prefix: None,
            window: (4, 0),
            exported_objects: 2,
        },
        Row {
            case: "a window that selects no event at all",
            prefix: Some("empty-window"),
            stream_prefix: None,
            window: (12, 10),
            exported_objects: 0,
        },
    ];

    for row in &rows {
        let emulator = Emulator::new();
        let case = row.case;
        let clock = plant(&emulator, case).await;

        let (from_hours, to_hours) = row.window;
        let mut create = json!({
            "logGroupName": GROUP,
            "from": clock - from_hours * HOUR,
            "to": clock - to_hours * HOUR,
            "destination": BUCKET,
        });
        if let Some(prefix) = row.prefix {
            create["destinationPrefix"] = json!(prefix);
        }
        if let Some(prefix) = row.stream_prefix {
            create["logStreamNamePrefix"] = json!(prefix);
        }
        let task_id = started(&emulator, case, &create).await;

        let status = emulator.awaited_terminal_status(case, &task_id).await;
        assert_eq!(
            status, "COMPLETED",
            "case {case:?}: the export must complete, got {status}"
        );

        // One trailing separator and no more stands between the prefix and the rest of the
        // key, whether or not the prefix the create named ended in one.
        let prefix = row.prefix.unwrap_or(DEFAULT_PREFIX);
        let check_key = format!("{}/{WRITE_TEST_OBJECT}", prefix.trim_end_matches('/'));
        let keys = emulator.keys_under(case, BUCKET, prefix).await;
        assert!(
            keys.contains(&check_key),
            "case {case:?}: the export's write check must be at {check_key}, got {keys:?}"
        );

        let body = emulator.object(case, BUCKET, &check_key).await;
        assert_eq!(
            String::from_utf8_lossy(&body),
            WRITE_TEST_BODY,
            "case {case:?}: the write check at {check_key} must report the check it is"
        );
        assert!(
            !body.starts_with(&GZIP_MAGIC),
            "case {case:?}: the write check at {check_key} is plain text, not a gzip frame, \
             got {body:?}"
        );

        let exported = emulator.exported_keys_under(case, BUCKET, prefix).await;
        assert_eq!(
            exported.len(),
            row.exported_objects,
            "case {case:?}: the window selects {} stream(s) of log data, got {exported:?}",
            row.exported_objects
        );
        // The check is the only object the export writes that is not exported log data, so
        // the two sets together are everything under the prefix.
        assert_eq!(
            keys.len(),
            exported.len() + 1,
            "case {case:?}: the prefix holds the exported data and the one write check, \
             got {keys:?}"
        );
    }
}

/// A `logStreamNamePrefix` that matches no stream of the group still exports: the check is
/// what shows that the export reached the destination, under a prefix no log data landed
/// under.
#[tokio::test]
async fn an_export_whose_stream_prefix_matches_nothing_still_leaves_its_write_check() {
    let case = "a stream prefix no stream of the group matches";
    let emulator = Emulator::new();
    let clock = plant(&emulator, case).await;
    let prefix = "no-stream";

    let task_id = started(
        &emulator,
        case,
        &json!({
            "logGroupName": GROUP,
            "from": clock - 4 * HOUR,
            "to": clock,
            "destination": BUCKET,
            "destinationPrefix": prefix,
            "logStreamNamePrefix": "absent/",
        }),
    )
    .await;

    let status = emulator.awaited_terminal_status(case, &task_id).await;
    assert_eq!(
        status, "COMPLETED",
        "case {case:?}: the export must complete, got {status}"
    );
    assert_eq!(
        emulator.keys_under(case, BUCKET, prefix).await,
        vec![format!("{prefix}/{WRITE_TEST_OBJECT}")],
        "case {case:?}: the prefix holds the write check and no exported log data"
    );
}

/// Two exports into ONE prefix leave ONE write check: the key is the prefix's own, so the
/// second export's check replaces the first's rather than accumulating beside it, while each
/// export's log data stays under its own task id.
#[tokio::test]
async fn two_exports_into_one_prefix_leave_one_write_check() {
    let case = "two sequential exports into the same prefix";
    let emulator = Emulator::new();
    let clock = plant(&emulator, case).await;
    let prefix = "shared";

    let mut task_ids = Vec::new();
    // "Each account can only have one active (RUNNING or PENDING) export task at a time", so
    // the second create waits for the first task to settle.
    for _ in 0..2 {
        let task_id = started(
            &emulator,
            case,
            &json!({
                "logGroupName": GROUP,
                "from": clock - 4 * HOUR,
                "to": clock,
                "destination": BUCKET,
                "destinationPrefix": prefix,
            }),
        )
        .await;
        let status = emulator.awaited_terminal_status(case, &task_id).await;
        assert_eq!(
            status, "COMPLETED",
            "case {case:?}: each export must complete, got {status}"
        );
        task_ids.push(task_id);
    }

    let keys = emulator.keys_under(case, BUCKET, prefix).await;
    let checks: Vec<&String> = keys
        .iter()
        .filter(|key| key.ends_with(WRITE_TEST_OBJECT))
        .collect();
    assert_eq!(
        checks,
        vec![&format!("{prefix}/{WRITE_TEST_OBJECT}")],
        "case {case:?}: the write check is keyed by the prefix alone, got {keys:?}"
    );
    for task_id in &task_ids {
        assert!(
            keys.iter().any(|key| key.contains(task_id)),
            "case {case:?}: each export's own log data is still under {prefix}/, got {keys:?}"
        );
    }
}

/// `GROUP`'s streams and [`PLANTED`] events in a freshly built emulator, with `BUCKET`
/// created to export into. Returns the clock the planted timestamps are relative to.
async fn plant(emulator: &Emulator, case: &str) -> i64 {
    emulator.create_bucket(case, BUCKET).await;
    emulator
        .logs_ok(case, "CreateLogGroup", &json!({"logGroupName": GROUP}))
        .await;
    let clock = now_ms();
    let mut streams: Vec<&str> = PLANTED.iter().map(|(stream, _, _)| *stream).collect();
    streams.sort_unstable();
    streams.dedup();
    for stream in streams {
        emulator
            .logs_ok(
                case,
                "CreateLogStream",
                &json!({"logGroupName": GROUP, "logStreamName": stream}),
            )
            .await;
        let events: Vec<Value> = PLANTED
            .iter()
            .filter(|(planted, _, _)| *planted == stream)
            .map(|(_, hours_before, message)| {
                json!({"timestamp": clock - hours_before * HOUR, "message": message})
            })
            .collect();
        emulator
            .logs_ok(
                case,
                "PutLogEvents",
                &json!({
                    "logGroupName": GROUP,
                    "logStreamName": stream,
                    "logEvents": Value::Array(events),
                }),
            )
            .await;
    }
    clock
}

/// The id of the export task `create` starts.
async fn started(emulator: &Emulator, case: &str, create: &Value) -> String {
    let answer = emulator.logs_ok(case, "CreateExportTask", create).await;
    answer["taskId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a started export reports a taskId, got {answer}"))
        .to_string()
}
