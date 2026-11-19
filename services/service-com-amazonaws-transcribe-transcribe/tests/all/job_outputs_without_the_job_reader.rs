// SPDX-License-Identifier: CC0-1.0

//! What a finished job's reported output location serves when the family's per-job
//! READER is never called.
//!
//! THE RULE THESE ROWS ASSERT: the document a finished job owes the location it reports
//! is written there when the job FINISHES, so a client that directed the output to a
//! bucket and key of its own — and therefore knows that address from its own start
//! request — fetches the document from the object store without ever reading the job
//! back. The location is documented as something to fetch from
//! (`Transcript$TranscriptFileUri` is "The Amazon S3 location of your transcript. You
//! can use this URI to access or download your transcript",
//! `smithy_query(query="doc", shape="Transcript", member="TranscriptFileUri")`, and
//! every other output member of this service documents its own location the same way),
//! and nothing in that contract makes the document conditional on which call observed
//! the job finishing. The sibling suite (`crate::job_output_objects`) asserts the same
//! rule through the family's own `Get`; these rows assert it for the OTHER calls that
//! observe a whole family — a listing of it, and a later start, which counts the jobs
//! being processed and makes way for the record it stores.
//!
//! The rows differ along what the rule generalises over: which family started the job
//! (all four), which call observes it finishing (the family's listing, a later start of
//! the family), where the request directed the output (a key naming a file, a key naming
//! a folder, a Call Analytics `OutputLocation` in two of its documented forms, the
//! per-job prefix a Medical Scribe job's bucket gets) and how many documents the job
//! therefore owes (one transcript, or a transcript and a clinical note).
//!
//! The emulator mounted here holds Amazon S3 beside Transcribe on ONE shared engine,
//! and the media every row names is staged in it first, reusing the harness the
//! media-resolution suite stands up (`crate::media_object_resolution`) and the family
//! table and output bucket of the sibling suite (`crate::job_output_objects`) rather
//! than building a second of either.

use serde_json::Value;
use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::job_output_objects::{
    Family, OUTPUT_BUCKET, Row, assert_document, given_output_buckets, start_request,
};
use crate::media_object_resolution::{
    BUCKET, STAGED_KEYS, call, emulator, given_staged_media, s3, s3_response,
};

/// How long a poll waits between two observing calls, and how many it makes before
/// giving up: together comfortably past the spans of
/// `crate::state::JOB_TRANSITION_DELAY` a job needs to finish, even on a loaded machine.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);
/// See [`POLL_INTERVAL`].
const POLLS_FOR_THE_DOCUMENT: usize = 200;

/// The `Content-Type` a JSON output document is served with.
const JSON_CONTENT_TYPE: &str = "application/json";

/// Which call observes the job finishing — and so is the one that must leave the
/// document at the location the job reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Observer {
    /// The family's own listing, which reports every job of this Region.
    Listing,
    /// A later start of the same family, which counts the jobs being processed and
    /// makes way for the record it stores.
    LaterStart,
}

/// One row: a job, the call that observes it finishing, and the object-store paths the
/// documents it owes must be served from.
struct Case {
    /// The job this row starts, spelled as the sibling suite's rows are.
    row: Row,
    observer: Observer,
    /// Every path the finished job's documents must be served from, built from the
    /// job's own name — the address the caller already knows, since its start request
    /// is what directed the output there.
    paths: fn(&str) -> Vec<String>,
}

/// The family's listing operation, the one call that reports every job of a family.
fn list_operation(family: Family) -> &'static str {
    match family {
        Family::Transcription => "ListTranscriptionJobs",
        Family::Medical => "ListMedicalTranscriptionJobs",
        Family::CallAnalytics => "ListCallAnalyticsJobs",
        Family::Scribe => "ListMedicalScribeJobs",
    }
}

/// A start request of `family` for a job of its own, directed at an output location no
/// row's job uses: a row whose observer is a later START needs one more job of the
/// family to start, and that job's own documents must not land on the row's.
fn probe_request(family: Family, name: &str) -> Value {
    let mut request = family.start_request(name);
    let output = match family {
        // `OutputLocation` is this family's whole-URI form of an output location.
        Family::CallAnalytics => json!({"OutputLocation": format!("s3://{OUTPUT_BUCKET}/probe/")}),
        // A Medical Scribe job takes no output key: its bucket gets a prefix named for
        // the job (`crate::state::medical_scribe_output_uri`).
        Family::Scribe => json!({"OutputBucketName": OUTPUT_BUCKET}),
        Family::Transcription | Family::Medical => json!({
            "OutputBucketName": OUTPUT_BUCKET,
            "OutputKey": format!("probe/{name}.json"),
        }),
    };
    let Value::Object(members) = output else {
        panic!("the probe's output members are an object");
    };
    for (member, value) in members {
        request[member] = value;
    }
    request
}

/// The rows: every family, both observing calls, and each documented form of a
/// caller-named output location.
fn cases() -> Vec<Case> {
    vec![
        Case {
            row: Row {
                case: "a transcription job observed finishing by the family's listing, \
                       its OutputKey naming the output file",
                family: Family::Transcription,
                name: "unread-listed",
                output: || {
                    json!({
                        "OutputBucketName": OUTPUT_BUCKET,
                        "OutputKey": "unread-listed/chosen.json",
                    })
                },
                buckets: &[OUTPUT_BUCKET],
                locations: 1,
            },
            observer: Observer::Listing,
            paths: |_| vec![format!("/{OUTPUT_BUCKET}/unread-listed/chosen.json")],
        },
        Case {
            row: Row {
                case: "a transcription job observed finishing by a later start, its \
                       OutputKey naming a folder",
                family: Family::Transcription,
                name: "unread-started-over",
                output: || {
                    json!({"OutputBucketName": OUTPUT_BUCKET, "OutputKey": "unread-started/"})
                },
                buckets: &[OUTPUT_BUCKET],
                locations: 1,
            },
            observer: Observer::LaterStart,
            paths: |name| vec![format!("/{OUTPUT_BUCKET}/unread-started/{name}.json")],
        },
        Case {
            row: Row {
                case: "a medical transcription job observed finishing by the family's \
                       listing",
                family: Family::Medical,
                name: "unread-medical-listed",
                output: || {
                    json!({"OutputBucketName": OUTPUT_BUCKET, "OutputKey": "medical-listed/"})
                },
                buckets: &[OUTPUT_BUCKET],
                locations: 1,
            },
            observer: Observer::Listing,
            paths: |name| vec![format!("/{OUTPUT_BUCKET}/medical-listed/{name}.json")],
        },
        Case {
            row: Row {
                case: "a medical transcription job observed finishing by a later start",
                family: Family::Medical,
                name: "unread-medical-started-over",
                output: || {
                    json!({
                        "OutputBucketName": OUTPUT_BUCKET,
                        "OutputKey": "medical-started/chosen.json",
                    })
                },
                buckets: &[OUTPUT_BUCKET],
                locations: 1,
            },
            observer: Observer::LaterStart,
            paths: |_| vec![format!("/{OUTPUT_BUCKET}/medical-started/chosen.json")],
        },
        Case {
            row: Row {
                case: "a Call Analytics job observed finishing by the family's listing, \
                       its OutputLocation naming a folder",
                family: Family::CallAnalytics,
                name: "unread-analytics-listed",
                output: || json!({"OutputLocation": format!("s3://{OUTPUT_BUCKET}/calls-listed/")}),
                buckets: &[OUTPUT_BUCKET],
                locations: 1,
            },
            observer: Observer::Listing,
            paths: |name| vec![format!("/{OUTPUT_BUCKET}/calls-listed/{name}.json")],
        },
        Case {
            row: Row {
                case: "a Call Analytics job observed finishing by a later start, its \
                       OutputLocation naming the output file",
                family: Family::CallAnalytics,
                name: "unread-analytics-started-over",
                output: || {
                    json!({
                        "OutputLocation":
                            format!("s3://{OUTPUT_BUCKET}/calls-started/chosen-call.json"),
                    })
                },
                buckets: &[OUTPUT_BUCKET],
                locations: 1,
            },
            observer: Observer::LaterStart,
            paths: |_| vec![format!("/{OUTPUT_BUCKET}/calls-started/chosen-call.json")],
        },
        Case {
            row: Row {
                case: "a Medical Scribe job observed finishing by the family's listing, \
                       which owes a transcript and a clinical note",
                family: Family::Scribe,
                name: "unread-scribe-listed",
                output: || json!({"OutputBucketName": OUTPUT_BUCKET}),
                buckets: &[OUTPUT_BUCKET],
                locations: 2,
            },
            observer: Observer::Listing,
            paths: |name| {
                vec![
                    format!("/{OUTPUT_BUCKET}/{name}/transcript.json"),
                    format!("/{OUTPUT_BUCKET}/{name}/summary.json"),
                ]
            },
        },
        Case {
            row: Row {
                case: "a Medical Scribe job observed finishing by a later start",
                family: Family::Scribe,
                name: "unread-scribe-started-over",
                output: || json!({"OutputBucketName": OUTPUT_BUCKET}),
                buckets: &[OUTPUT_BUCKET],
                locations: 2,
            },
            observer: Observer::LaterStart,
            paths: |name| {
                vec![
                    format!("/{OUTPUT_BUCKET}/{name}/transcript.json"),
                    format!("/{OUTPUT_BUCKET}/{name}/summary.json"),
                ]
            },
        },
    ]
}

/// One GET of the object store through the shared helper, keeping the `Content-Type`
/// the object is served with: what is asserted here is that the stored document is
/// SERVED, and the media type it is served as is part of that
/// (`crate::media_object_resolution::s3_response`).
async fn served(router: &MultiServiceRouter, path: &str) -> (u16, Option<String>, bytes::Bytes) {
    let (status, headers, body) = s3_response(router, "GET", path, &[]).await;
    let content_type = headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    (status, content_type, body)
}

/// Make the row's observing call once — the only call that may observe the job, since
/// the family's own reader is what these rows do without.
async fn observe(router: &MultiServiceRouter, case: &Case, attempt: usize) {
    match case.observer {
        Observer::Listing => {
            call(router, list_operation(case.row.family), json!({}))
                .await
                .ok(case.row.case);
        }
        Observer::LaterStart => {
            let name = format!("{}-probe-{attempt}", case.row.name);
            call(
                router,
                case.row.family.start(),
                probe_request(case.row.family, &name),
            )
            .await
            .ok(case.row.case);
        }
    }
}

/// Every document a finished job owes the location its own request directed the output
/// to is served there, with the family's per-job reader never called.
#[tokio::test]
async fn a_finished_job_serves_its_documents_without_being_read_back() {
    let router = emulator();
    given_staged_media(&router).await;
    assert!(
        !STAGED_KEYS.is_empty(),
        "the media the rows name is staged in {BUCKET}"
    );

    for case in &cases() {
        given_output_buckets(&router, &case.row).await;
        call(&router, case.row.family.start(), start_request(&case.row))
            .await
            .ok(case.row.case);

        // Poll the ROW'S OWN observing call until every document the job owes is served:
        // the condition is the document being there, never a wall-clock reading, and the
        // family's own reader is never called.
        let paths = (case.paths)(case.row.name);
        let mut served_now = Vec::new();
        for attempt in 0..POLLS_FOR_THE_DOCUMENT {
            observe(&router, case, attempt).await;
            served_now.clear();
            for path in &paths {
                served_now.push(served(&router, path).await);
            }
            if served_now.iter().all(|(status, _, _)| *status == 200) {
                break;
            }
            std::thread::sleep(POLL_INTERVAL);
        }

        for (path, (status, content_type, body)) in paths.iter().zip(&served_now) {
            assert_eq!(
                *status, 200,
                "case {:?}: the location the job reports must serve its document once the \
                 job has finished, got HTTP {status} from {path} with body {:?}",
                case.row.case,
                String::from_utf8_lossy(body)
            );
            assert_eq!(
                content_type.as_deref(),
                Some(JSON_CONTENT_TYPE),
                "case {:?}: the document at {path} is served as the JSON it is, got \
                 {content_type:?}",
                case.row.case
            );
            // The document a location owes is the same document whichever call left it
            // there, so WHAT is at it is asserted by the sibling suite's own check
            // (`crate::job_output_objects::assert_document`) rather than a second copy.
            assert_document(case.row.case, case.row.name, path, body);
        }
        assert_eq!(
            served_now.len(),
            case.row.locations,
            "case {:?}: expected {} document(s) at {paths:?}",
            case.row.case,
            case.row.locations
        );
    }
}

/// The document is owed when the job FINISHES and not before: a listing made straight
/// after the start leaves the location the finished job will report empty, exactly as
/// the family's own reader does.
#[tokio::test]
async fn a_listing_writes_nothing_for_a_job_still_being_worked_on() {
    let router = emulator();
    given_staged_media(&router).await;
    let row = Row {
        case: "a job listed straight after it was started",
        family: Family::Transcription,
        name: "listed-while-running",
        output: || {
            json!({"OutputBucketName": OUTPUT_BUCKET, "OutputKey": "listed-pending/out.json"})
        },
        buckets: &[OUTPUT_BUCKET],
        locations: 1,
    };
    given_output_buckets(&router, &row).await;
    call(&router, row.family.start(), start_request(&row))
        .await
        .ok(row.case);

    let listing = call(&router, list_operation(row.family), json!({})).await;
    let summaries = listing.ok(row.case)["TranscriptionJobSummaries"].clone();
    assert!(
        summaries.is_array(),
        "case {:?}: the listing reports this family's jobs, got {summaries:?}",
        row.case
    );

    let path = format!("/{OUTPUT_BUCKET}/listed-pending/out.json");
    let (status, _, body) = served(&router, &path).await;
    assert_eq!(
        status, 404,
        "case {:?}: nothing is stored at {path} while the job is still being worked on, \
         got HTTP {status} with body {:?}",
        row.case,
        String::from_utf8_lossy(&body)
    );

    // …and once the job has finished, the very same listing leaves the document there.
    for _ in 0..POLLS_FOR_THE_DOCUMENT {
        call(&router, list_operation(row.family), json!({}))
            .await
            .ok(row.case);
        if served(&router, &path).await.0 == 200 {
            break;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    let (status, content_type, body) = served(&router, &path).await;
    assert_eq!(
        status, 200,
        "case {:?}: the finished job's location serves its transcript, got HTTP {status} \
         with body {:?}",
        row.case,
        String::from_utf8_lossy(&body)
    );
    assert_eq!(
        content_type.as_deref(),
        Some(JSON_CONTENT_TYPE),
        "case {:?}: the document at {path} is served as the JSON it is, got \
         {content_type:?}",
        row.case
    );
    assert_document(row.case, row.name, &path, &body);

    // The object store holds it, so a plain GET of the same address serves the same
    // bytes a second time: nothing about the write depended on the poll.
    let (again, same_body) = s3(&router, "GET", &path, &[]).await;
    assert_eq!(
        (again, same_body),
        (status, body),
        "case {:?}: the stored document is served unchanged on a second fetch",
        row.case
    );
}
