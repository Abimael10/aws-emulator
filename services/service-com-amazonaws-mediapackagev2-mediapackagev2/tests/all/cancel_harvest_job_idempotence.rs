// SPDX-License-Identifier: CC0-1.0

//! What a REPEATED `CancelHarvestJob` is answered, and which statuses the
//! ineligible-for-cancellation refusal is reserved for.
//!
//! RULE: `CancelHarvestJob` is `@idempotent`
//! (`smithy_query(query="operation",operation="CancelHarvestJob",model_path="services/service-com-amazonaws-mediapackagev2-mediapackagev2/smithy-model/com-amazonaws-mediapackagev2-mediapackagev2.json")`
//! — `idempotent: true`, `PUT`, HTTP 200, output `CancelHarvestJobResponse` with no
//! members), so a cancellation of a job that is ALREADY in the status the call asks for is
//! the same request as the one that put it there: it is answered the same empty 200 and
//! commits nothing, leaving the job's status, entity tag and modification instant as the
//! first cancellation left them. The `ValidationException` (400) /
//! `HARVEST_JOB_INELIGIBLE_FOR_CANCELLATION` refusal belongs to a job whose WORK has ended
//! — `COMPLETED` or `FAILED`, the `HarvestJobStatus` values a cancellation cannot reach —
//! never to the status a cancellation itself writes.
//!
//! The rows span what the rule generalises over rather than one recorded request: the live
//! status the first cancellation arrives in (`QUEUED` and `IN_PROGRESS`, the two
//! `HarvestJobStatus` values a job can still be cancelled from), the shape of the window it
//! was scheduled for (opening later, open now, opened in the past), how many times the
//! cancellation is repeated, and what entity-tag precondition each repeat presents — a
//! no-op write still resolves the shared `x-amzn-update-if-match` precondition, so a repeat
//! presenting a tag that is not the committed one is still the 409 a conditional write
//! exists to report.
//!
//! The refusal side is asserted in the same file over its own set of closed windows, so the
//! accepted repeats above stand against a real refusal rather than against an operation
//! that accepts everything.

use serde_json::{json, Value};
use service_com_amazonaws_mediapackagev2_mediapackagev2::state::VALIDATION_HARVEST_JOB_INELIGIBLE_FOR_CANCELLATION;

// The crate's one control-plane client and error-envelope reader, the one
// `CreateHarvestJob` body with its schedule clock, and the shared precondition's header,
// sentence, reader and harvest-job waiters — this suite drives the router through them
// rather than growing a second copy of any of it.
use crate::channel_lifecycle::{error_type, minimal_endpoint, Emulator};
use crate::rejection_rule_and_negative_paths::{epoch_seconds_from_now, harvest_body};
use crate::shared_entity_tag_precondition::{
    ended_job, message, read, settle_job, IF_MATCH, STALE_ETAG_MESSAGE,
};

/// The channel and origin endpoint every row builds inside a channel group of its own, and
/// the manifest the endpoint publishes (so the one a harvest job may harvest:
/// [`minimal_endpoint`] publishes exactly `index`).
const CHANNEL: &str = "cij-channel";
/// See [`CHANNEL`].
const ENDPOINT: &str = "cij-endpoint";
/// See [`CHANNEL`].
const JOB: &str = "cij-job";
/// See [`CHANNEL`].
const MANIFEST: &str = "index";

/// The `HarvestJobStatus` values a job can still be cancelled from, and the one a
/// cancellation writes (model: `QUEUED | IN_PROGRESS | CANCELLED | COMPLETED | FAILED`).
const QUEUED: &str = "QUEUED";
/// See [`QUEUED`].
const IN_PROGRESS: &str = "IN_PROGRESS";
/// See [`QUEUED`].
const CANCELLED: &str = "CANCELLED";
/// See [`QUEUED`].
const FAILED: &str = "FAILED";

/// What precondition a repeated cancellation presents in `x-amzn-update-if-match`.
#[derive(Debug, Clone, Copy)]
enum Presented {
    /// No header at all: the repeat is unconditional.
    Absent,
    /// The tag the store holds the job at, which the first cancellation committed.
    Committed,
    /// A tag the job is not at, which a conditional write must lose on even when the work
    /// it asks for is already done.
    NotCommitted,
}

/// One row: a harvest job created for a window of a given shape, cancelled once the job
/// holds one of the live statuses, then cancelled again under each presented precondition.
struct Row {
    /// What makes this row different from the others.
    label: &'static str,
    /// The scheduled window, in seconds from the instant the row runs.
    start_offset: f64,
    end_offset: f64,
    /// Whether the row waits for the clock to move the job past `QUEUED` before the first
    /// cancellation, which is what makes the `IN_PROGRESS` rows that status for certain.
    settle: bool,
    /// The live statuses the job may hold when the first cancellation arrives: an
    /// unsettled row is read while the `QUEUED` -> `IN_PROGRESS` transition may or may not
    /// have fallen due, and either is a job that can still be cancelled.
    live: &'static [&'static str],
    /// The preconditions the repeats present, in order.
    repeats: &'static [Presented],
}

/// The rows: the live status the cancellation arrives in, the window shape it was
/// scheduled for, and the preconditions its repeats present.
const ROWS: &[Row] = &[
    Row {
        label: "a window that opens an hour from now, cancelled before it does",
        start_offset: 3600.0,
        end_offset: 7200.0,
        settle: false,
        live: &[QUEUED, IN_PROGRESS],
        repeats: &[Presented::Absent, Presented::Absent, Presented::Absent],
    },
    Row {
        label: "a window open now, cancelled as soon as the job exists",
        start_offset: 0.0,
        end_offset: 3600.0,
        settle: false,
        live: &[QUEUED, IN_PROGRESS],
        repeats: &[Presented::Absent, Presented::Committed],
    },
    Row {
        label: "a window open now, cancelled once the clock has moved the job on",
        start_offset: 0.0,
        end_offset: 3600.0,
        settle: true,
        live: &[IN_PROGRESS],
        repeats: &[Presented::Committed, Presented::Absent, Presented::Committed],
    },
    Row {
        label: "a long window that opened ten minutes ago, cancelled while harvesting",
        start_offset: -600.0,
        end_offset: 86_400.0,
        settle: true,
        live: &[IN_PROGRESS],
        repeats: &[Presented::NotCommitted, Presented::Absent],
    },
    Row {
        label: "a window a day long that opens tomorrow, repeated once",
        start_offset: 86_400.0,
        end_offset: 172_800.0,
        settle: false,
        live: &[QUEUED, IN_PROGRESS],
        repeats: &[Presented::NotCommitted, Presented::Committed],
    },
];

/// Build the group, channel, origin endpoint and harvest job of one row, and return the
/// path the job's reader and `CancelHarvestJob` share (the operation is a `PUT` on the
/// job's own path, which `GetHarvestJob` reads with a `GET`).
async fn fresh_job(
    emulator: &Emulator,
    case: &str,
    group: &str,
    start_offset: f64,
    end_offset: f64,
) -> String {
    let (status, body) = emulator.create_group(group, json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateChannelGroup {group} expected 200, got {status} {body}"
    );
    let (status, body) = emulator.create_channel(group, CHANNEL, json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateChannel in {group} expected 200, got {status} {body}"
    );
    let (status, body) = emulator
        .create_endpoint(group, CHANNEL, ENDPOINT, minimal_endpoint("TS"))
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateOriginEndpoint in {group} expected 200, got {status} {body}"
    );
    let endpoint_path = format!("/channelGroup/{group}/channel/{CHANNEL}/originEndpoint/{ENDPOINT}");
    let base = epoch_seconds_from_now(0.0);
    let mut request = harvest_body(base + start_offset, base + end_offset, &[MANIFEST]);
    request["HarvestJobName"] = json!(JOB);
    let (status, body) = emulator
        .call("POST", &format!("{endpoint_path}/harvestJob"), Some(request))
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateHarvestJob in {group} expected 200, got {status} {body}"
    );
    format!("{endpoint_path}/harvestJob/{JOB}")
}

/// Assert one accepted cancellation: HTTP 200 and the modelled response, which has no
/// members at all — so the answer carries either no body or an empty object.
fn assert_empty_success(case: &str, status: u16, answer: &Value) {
    assert_eq!(
        status, 200,
        "case {case:?}: an accepted cancellation expected HTTP 200, got {status} {answer}"
    );
    let members = match answer {
        Value::Null => 0,
        Value::Object(map) => map.len(),
        other => panic!("case {case:?}: expected an empty response, got {other}"),
    };
    assert_eq!(
        members, 0,
        "case {case:?}: CancelHarvestJobResponse has no members, got {answer}"
    );
}

/// RULE: a cancellation of a job that is already `CANCELLED` is the same request as the one
/// that cancelled it — the same empty 200, and nothing committed — whichever live status
/// the first cancellation found the job in and however many times the retry arrives, while
/// a repeat presenting an entity tag that is not the committed one is still refused 409.
#[tokio::test]
async fn a_repeated_cancellation_is_answered_the_same_success_and_commits_nothing() {
    let emulator = Emulator::new();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.label;
        let path = fresh_job(
            &emulator,
            case,
            &format!("cij-{index}"),
            row.start_offset,
            row.end_offset,
        )
        .await;
        if row.settle {
            settle_job(&emulator, &path).await;
        }

        // The job is still cancellable when the first cancellation arrives: it holds a live
        // status, not one the clock has ended.
        let live = read(&emulator, case, &path).await;
        let held = live["Status"].as_str().unwrap_or_default().to_string();
        assert!(
            row.live.contains(&held.as_str()),
            "case {case:?}: the job must hold one of {:?} before the cancellation, got {held:?}",
            row.live
        );

        let (status, answer) = emulator.call("PUT", &path, None).await;
        assert_empty_success(&format!("{case}: the first cancellation"), status, &answer);
        let before = read(&emulator, case, &path).await;
        assert_eq!(
            before["Status"],
            json!(CANCELLED),
            "case {case:?}: the cancelled job must report {CANCELLED}, got {before}"
        );
        let committed = before["ETag"].as_str().unwrap_or_default().to_string();
        assert!(
            !committed.is_empty(),
            "case {case:?}: the reader must report the job's committed entity tag, got {before}"
        );

        for (repeat, presented) in row.repeats.iter().enumerate() {
            let case = format!("{case}: repeat {} presenting {presented:?}", repeat + 1);
            // A tag the job is not at: the committed one with one more character on the
            // end, which a comparison matching a PREFIX would wrongly accept.
            let not_committed = format!("{committed}0");
            let headers: Vec<(&str, &str)> = match presented {
                Presented::Absent => Vec::new(),
                Presented::Committed => vec![(IF_MATCH, committed.as_str())],
                Presented::NotCommitted => vec![(IF_MATCH, not_committed.as_str())],
            };
            let (status, answer) = emulator
                .call_with_headers("PUT", &path, None, &headers)
                .await;

            match presented {
                Presented::Absent | Presented::Committed => {
                    assert_empty_success(&case, status, &answer);
                }
                Presented::NotCommitted => {
                    assert_eq!(
                        status, 409,
                        "case {case:?}: a tag that is not the committed one must be refused \
                         409, got {status} {answer}"
                    );
                    assert_eq!(
                        error_type(&answer),
                        "ConflictException",
                        "case {case:?}: expected ConflictException, got {answer}"
                    );
                    assert_eq!(
                        answer["ConflictExceptionType"],
                        json!("CONFLICTING_OPERATION"),
                        "case {case:?}: expected CONFLICTING_OPERATION, got {answer}"
                    );
                    assert_eq!(
                        message(&answer),
                        STALE_ETAG_MESSAGE,
                        "case {case:?}: expected the fixed sentence for a stale entity tag, \
                         got {answer}"
                    );
                }
            }

            // Whichever answer the repeat earned, it committed nothing: the job is exactly
            // what the FIRST cancellation left — same status, same entity tag, same
            // modification instant, same everything the reader projects.
            let after = read(&emulator, &case, &path).await;
            assert_eq!(
                after, before,
                "case {case:?}: a repeat must leave the job exactly as the first \
                 cancellation left it; got {after} against {before}"
            );
        }
    }
}

/// The windows the refusal rows are scheduled for: each had already CLOSED when its job was
/// created, so the clock ends the job of its own accord and nothing this test writes puts it
/// in the status the cancellation is refused for. The rows differ in how long ago the window
/// closed and in how long it was.
const CLOSED_WINDOWS: &[(&str, f64, f64)] = &[
    ("a window that closed an hour before the job", -7200.0, -3600.0),
    ("a one-second window that closed a day before the job", -86_401.0, -86_400.0),
];

/// RULE: the ineligible-for-cancellation refusal is reserved for a job whose WORK has ended
/// — a status a cancellation cannot reach — and it too commits nothing, however often the
/// caller retries.
#[tokio::test]
async fn cancelling_a_job_whose_work_has_ended_stays_refused_and_commits_nothing() {
    let emulator = Emulator::new();

    for (index, (label, start_offset, end_offset)) in CLOSED_WINDOWS.iter().enumerate() {
        let path = fresh_job(
            &emulator,
            label,
            &format!("cij-ended-{index}"),
            *start_offset,
            *end_offset,
        )
        .await;
        // Nobody touches the job: the clock alone ends it, which is what makes the refusal
        // below a property of the status rather than of this test's own writes.
        ended_job(&emulator, label, &path).await;
        let before = read(&emulator, label, &path).await;
        assert_eq!(
            before["Status"],
            json!(FAILED),
            "case {label:?}: the clock must have ended the job, got {before}"
        );

        for attempt in 1..=2 {
            let case = format!("{label}: cancellation attempt {attempt}");
            let (status, answer) = emulator.call("PUT", &path, None).await;
            assert_eq!(
                status, 400,
                "case {case:?}: a job whose work has ended must be refused 400, got {status} \
                 {answer}"
            );
            assert_eq!(
                error_type(&answer),
                "ValidationException",
                "case {case:?}: expected the modelled ValidationException, got {answer}"
            );
            assert_eq!(
                answer["ValidationExceptionType"],
                json!(VALIDATION_HARVEST_JOB_INELIGIBLE_FOR_CANCELLATION),
                "case {case:?}: expected {VALIDATION_HARVEST_JOB_INELIGIBLE_FOR_CANCELLATION}, \
                 got {answer}"
            );
            assert!(
                !message(&answer).is_empty(),
                "case {case:?}: the refusal must state the rule the member failed, got {answer}"
            );
            let after = read(&emulator, &case, &path).await;
            assert_eq!(
                after, before,
                "case {case:?}: a refused cancellation must leave the job exactly as it was; \
                 got {after} against {before}"
            );
        }
    }
}
