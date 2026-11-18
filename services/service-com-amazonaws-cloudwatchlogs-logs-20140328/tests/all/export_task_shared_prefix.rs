// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for SEVERAL export tasks landing in ONE destination prefix.
//!
//! CloudWatch Logs and Amazon S3 are mounted on one shared engine — the same assembly
//! `core/router` performs — so every object a case reads back is one the object store itself
//! holds, written by the export task over the in-process callback and not by anything here.
//!
//! RULE under test, as `CreateExportTask` states it: "You can export logs from multiple log
//! groups or multiple time ranges to the same S3 bucket. To separate log data for each export
//! task, specify a prefix to be used as the Amazon S3 key prefix for all exported objects",
//! and a task reaches `COMPLETED` only once the copy has been made. So each task writes the
//! events of its OWN window under the destination prefix, keyed so that it cannot overwrite
//! another task's objects, and by the time it reports `COMPLETED` its objects are readable —
//! which makes the prefix of a bucket several exports ran into the accumulation of all of
//! them.
//!
//! The suites next door cover one export each: `tests/all/export_task_objects.rs` which
//! events one window selects, `tests/all/export_task_write_test_marker.rs` the check an
//! export leaves in the prefix. This suite's subject is what SEVERAL of them leave in one
//! prefix, over windows that select one event, two events across two streams, and every event
//! the group holds — including the windows the earlier tasks already exported.
//!
//! Every case waits for the terminal status the way a client does — poll
//! `DescribeExportTasks` until it is terminal — and then reads the bucket, so nothing here
//! waits for a fixed span of the clock.

use std::io::Read;

use serde_json::{json, Value};

use crate::export_task_objects::Emulator;
use crate::harness::now_ms;

/// Milliseconds in one hour.
const HOUR: i64 = 60 * 60 * 1_000;

/// The one prefix every export of this suite is pointed at.
const PREFIX: &str = "shared/archive";

/// The events the group holds, as `(stream, HALF hours before the clock, message)`, so a
/// window can sit between two of them.
const PLANTED: &[(&str, i64, &str)] = &[
    ("app/one", 7, "OLDEST"),
    ("app/one", 5, "MIDDLE"),
    ("app/one", 3, "NEWEST"),
    ("sys/two", 5, "OTHER-STREAM"),
];

/// One export of the sequence: the window it names, and which planted messages it must then
/// have written and must not have written.
struct Export {
    case: &'static str,
    /// The window, as HALF hours either side of the clock.
    window: (i64, i64),
    exported: &'static [&'static str],
    withheld: &'static [&'static str],
}

/// The exported text of the objects under [`PREFIX`] that `task_id` wrote: every object keyed
/// by that task, gunzipped and concatenated.
///
/// Each task's objects are told apart by the task's own id, which is what the keys carry so
/// that two exports into one prefix do not overwrite each other.
async fn text_written_by(emulator: &Emulator, case: &str, bucket: &str, task_id: &str) -> String {
    let mut text = String::new();
    for key in emulator
        .exported_keys_under(case, bucket, PREFIX)
        .await
        .into_iter()
        .filter(|key| key.contains(task_id))
    {
        let body = emulator.object(case, bucket, &key).await;
        let mut decoded = Vec::new();
        flate2::read::GzDecoder::new(body.as_slice())
            .read_to_end(&mut decoded)
            .unwrap_or_else(|error| {
                panic!("case {case:?}: the object at {key} must be gzip: {error}")
            });
        text.push_str(&String::from_utf8_lossy(&decoded));
    }
    text
}

/// Three exports into ONE destination prefix: each lands its own window's events by the time
/// it reports `COMPLETED`, and leaves every earlier export's objects exactly as they were.
#[tokio::test]
async fn each_export_into_one_prefix_lands_its_own_window_and_keeps_the_earlier_ones() {
    let sequence: [Export; 3] = [
        Export {
            case: "the first export, over the oldest event alone",
            // [-4h, -3h)
            window: (-8, -6),
            exported: &["OLDEST"],
            withheld: &["MIDDLE", "NEWEST", "OTHER-STREAM"],
        },
        Export {
            case: "a second export, over a later window spanning two streams",
            // [-3h, -2h)
            window: (-6, -4),
            exported: &["MIDDLE", "OTHER-STREAM"],
            withheld: &["OLDEST", "NEWEST"],
        },
        Export {
            case: "a third export, over every event, including the two already exported",
            window: (-20, 2),
            exported: &["OLDEST", "MIDDLE", "NEWEST", "OTHER-STREAM"],
            withheld: &[],
        },
    ];

    let emulator = Emulator::new();
    let group = "export/shared-prefix";
    let bucket = "export-shared-prefix";
    let fixture = "the fixture the exports read";
    emulator.create_bucket(fixture, bucket).await;
    emulator
        .logs_ok(fixture, "CreateLogGroup", &json!({"logGroupName": group}))
        .await;
    let clock = now_ms();
    for stream in ["app/one", "sys/two"] {
        emulator
            .logs_ok(
                fixture,
                "CreateLogStream",
                &json!({"logGroupName": group, "logStreamName": stream}),
            )
            .await;
        let events: Vec<Value> = PLANTED
            .iter()
            .filter(|(planted, _, _)| *planted == stream)
            .map(|(_, half_hours_before, message)| {
                json!({
                    "timestamp": clock - half_hours_before * HOUR / 2,
                    "message": message,
                })
            })
            .collect();
        emulator
            .logs_ok(
                fixture,
                "PutLogEvents",
                &json!({
                    "logGroupName": group,
                    "logStreamName": stream,
                    "logEvents": Value::Array(events),
                }),
            )
            .await;
    }

    // What each settled export wrote, so every later one can be checked against it.
    let mut settled: Vec<(&'static str, String, &'static [&'static str], &'static [&'static str])> =
        Vec::new();
    for export in &sequence {
        let case = export.case;
        let (from_half_hours, to_half_hours) = export.window;
        let started = emulator
            .logs_ok(
                case,
                "CreateExportTask",
                &json!({
                    "logGroupName": group,
                    "from": clock + from_half_hours * HOUR / 2,
                    "to": clock + to_half_hours * HOUR / 2,
                    "destination": bucket,
                    "destinationPrefix": PREFIX,
                }),
            )
            .await;
        let task_id = started["taskId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a started export reports a taskId"))
            .to_string();
        assert!(
            settled.iter().all(|(_, earlier, _, _)| *earlier != task_id),
            "case {case:?}: each export is minted its own task id, got {task_id} again"
        );

        // Wait for the terminal status the way a client does, and read the destination with no
        // further wait: an export whose objects land after its status settles cannot pass.
        let status = emulator.awaited_terminal_status(case, &task_id).await;
        assert_eq!(
            status, "COMPLETED",
            "case {case:?}: the export must complete, got {status}"
        );
        settled.push((case, task_id, export.exported, export.withheld));

        // Every object under the prefix is keyed under it, and this export's own objects
        // carry its task id.
        for key in &emulator.keys_under(case, bucket, PREFIX).await {
            assert!(
                key.starts_with(&format!("{PREFIX}/")),
                "case {case:?}: every object the export writes is keyed under {PREFIX}/, got \
                 {key}"
            );
        }

        // Each settled export — this one and every earlier one — is still represented by its
        // own objects, holding its own window's events: a later export into the same prefix
        // neither overwrote them nor took their events.
        for (settled_case, settled_task, exported, withheld) in &settled {
            let text = text_written_by(&emulator, settled_case, bucket, settled_task).await;
            for message in *exported {
                assert!(
                    text.contains(message),
                    "case {settled_case:?} after {case:?}: the objects of {settled_task} must \
                     carry {message}, got {text:?}"
                );
            }
            for message in *withheld {
                assert!(
                    !text.contains(message),
                    "case {settled_case:?} after {case:?}: the objects of {settled_task} must \
                     not carry {message}, got {text:?}"
                );
            }
        }
    }

    // The prefix a client reads is therefore the accumulation of every export that ran into
    // it: each task's objects are there, and each event the group holds appears in it.
    let case = "the prefix every export ran into";
    let keys = emulator.exported_keys_under(case, bucket, PREFIX).await;
    for (_, task_id, _, _) in &settled {
        assert!(
            keys.iter().any(|key| key.contains(task_id)),
            "case {case:?}: the objects of {task_id} are still under {PREFIX}, got {keys:?}"
        );
    }
    let mut whole = String::new();
    for (_, task_id, _, _) in &settled {
        whole.push_str(&text_written_by(&emulator, case, bucket, task_id).await);
    }
    for (_, _, message) in PLANTED {
        assert!(
            whole.contains(message),
            "case {case:?}: the prefix must hold {message}, got {whole:?}"
        );
    }
}
