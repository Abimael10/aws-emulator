// SPDX-License-Identifier: CC0-1.0

//! End-to-end coverage of REPORTING a stored-video label-detection job over the wire
//! (`awsJson1_1`, `POST /` with `X-Amz-Target: RekognitionService.GetLabelDetection`),
//! dispatched through the same [`MultiServiceRouter`] the production emulator mounts, with
//! this service and the emulator's own Amazon S3 on ONE engine — the video a job runs on is
//! always an object of the object store.
//!
//! THE RULE UNDER TEST: a `Get…` operation of this API REPORTS the stored-video analysis job
//! its own `Start…` call recorded under the `JobId` it is given. It answers with the status
//! the clock owes that job, with the job's recorded identity (the video it runs on, the tag it
//! was started under) and with the request's own choices echoed back, and it serves the
//! analysis's results one page at a time. It writes NOTHING: the same job read twice answers
//! the same. An identifier naming no job of that analysis — never minted, or minted by
//! another analysis of the family — is refused with the modelled `ResourceNotFoundException`,
//! and a `NextToken` no page of that analysis minted with `InvalidPaginationTokenException`.
//!
//! Each test asserts that rule over a TABLE whose rows differ along what the rule generalises
//! over: which object the job's video names, which optional members the start and the read
//! carry and at what values (`JobTag`, `SortBy`, `AggregateBy`, `MaxResults`), how an
//! identifier fails to name a job, how a pagination token fails to name a position, and how
//! many results a page is cut from. The recorded case the issue was filed from — a started
//! label-detection job read until it reaches its terminal status — is the first test.
//!
//! WHAT A FINISHED JOB REPORTS: no video decoder runs in this process, so an analysis here
//! measures nothing of the media; a finished job reports its label array EMPTY rather than a
//! detection nothing measured, and the members that are measurements OF the media
//! (`VideoMetadata`) or of the model that made them (`LabelModelVersion`) stay absent. The
//! tests assert exactly that, so a later change that invents either is a failure.
//!
//! The one thing the clock owes a job is driven at instants the test HANDS the store
//! ([`video_report::report`]), so the status rule is decided by arithmetic and never by how
//! fast the host runs; the wire test waits for the CONDITION — it polls the job until it
//! reports its terminal status, under a deadline generous enough that a loaded host still
//! reaches it.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use service_com_amazonaws_rekognition_rekognitionservice::state;
use service_com_amazonaws_rekognition_rekognitionservice::state::video_jobs::{
    VideoJob, COMPLETION_SPAN,
};
use service_com_amazonaws_rekognition_rekognitionservice::state::video_report::{
    self, NotReported, MAX_RESULTS_PAGE_SIZE,
};
use service_com_amazonaws_rekognition_rekognitionservice::state::video_source::VideoLocation;
use smithy_server_runtime::MultiServiceRouter;

// The router harness with the object store mounted, the dispatch helpers and the reduced reply
// come from the suites that already own them rather than being built again here.
use crate::face_collections::{call, Reply};
use crate::face_detection::{harness_with_object_store, s3};

/// The bucket the videos of this suite live in.
const BUCKET: &str = "rekognition-label-detection-videos";

/// The keys this suite stages a video at, one per container format `Video` documents as
/// supported ("the supported file formats are .mp4, .mov and .avi",
/// `smithy_query(query="shape",shape="Video")`), with one under a key prefix because a key is
/// a key whatever it looks like. The bytes are opaque: nothing in this process decodes video.
const KEYS: &[&str] = &["clip.mp4", "recordings/2026/02/clip.mov", "clip.avi"];

/// The analysis a label-detection job is recorded under, and one of the family's others.
const LABEL_ANALYSIS: &str = "StartLabelDetection";
const FACE_ANALYSIS: &str = "StartFaceDetection";

/// The sorting and the aggregation this operation reports for a request that chose neither:
/// "the default sort is by TIMESTAMP" (`SortBy`) and "the default aggregation method is
/// TIMESTAMPS" (`GetLabelDetection`'s own documentation).
const DEFAULT_SORT_BY: &str = "TIMESTAMP";
const DEFAULT_AGGREGATE_BY: &str = "TIMESTAMPS";

/// The wording a `NextToken` this service's pages did not mint is refused with. Fixed text for
/// the class: no part of it comes from the request.
const INVALID_PAGINATION_TOKEN_MESSAGE: &str = "Invalid pagination token";

/// How long a job is given to reach its terminal status, and how often it is looked at while
/// it has not. The deadline is generous — far beyond the span the store owes the transition at
/// — because what is waited for is the CONDITION and not an elapsed time.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The emulator with the videos of [`KEYS`] staged in [`BUCKET`].
async fn seeded() -> MultiServiceRouter {
    let router = harness_with_object_store();
    assert_eq!(
        s3(&router, &format!("/{BUCKET}"), &[]).await,
        200,
        "the video bucket must be created first"
    );
    for key in KEYS {
        assert_eq!(
            s3(&router, &format!("/{BUCKET}/{key}"), b"stored-video-object").await,
            200,
            "the video object {key:?} must be stored"
        );
    }
    router
}

/// `Video` naming the staged object `key`.
fn staged_video(key: &str) -> Value {
    json!({ "S3Object": { "Bucket": BUCKET, "Name": key } })
}

async fn start_labels(router: &MultiServiceRouter, body: Value) -> Reply {
    call(router, LABEL_ANALYSIS, body).await
}

async fn get_labels(router: &MultiServiceRouter, body: Value) -> Reply {
    call(router, "GetLabelDetection", body).await
}

/// The `JobId` a start that was carried out answers with.
fn job_id_of(case: &str, reply: &Reply) -> String {
    assert_eq!(
        reply.status, 200,
        "case {case:?}: the start must be carried out, got {} with {:?}",
        reply.status, reply.body
    );
    reply.string("JobId").unwrap_or_else(|| {
        panic!("case {case:?}: a start answers with a JobId, got {:?}", reply.body)
    })
}

/// What every refusal of a read must carry: the status, the envelope's `__type`, the body's own
/// `Code` and the class's wording.
fn assert_refusal(case: &str, reply: &Reply, code: &str, message: &str) {
    assert_eq!(
        reply.status, 400,
        "case {case:?}: expected HTTP 400, got {} with {:?}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.error_code(),
        code,
        "case {case:?}: expected __type {code}, got {:?}",
        reply.body
    );
    assert_eq!(
        reply.string("Code").as_deref(),
        Some(code),
        "case {case:?}: expected the body's own Code {code}, got {:?}",
        reply.body
    );
    assert_eq!(
        reply.string("Message").as_deref(),
        Some(message),
        "case {case:?}: expected the message {message:?}, got {:?}",
        reply.body
    );
}

/// Read the job `body` names until it reports its terminal status, or until
/// [`SETTLE_DEADLINE`] has passed without it doing so.
async fn poll_until_succeeded(router: &MultiServiceRouter, case: &str, body: &Value) -> Reply {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let reply = get_labels(router, body.clone()).await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: reading a job the service started must answer, got {} with {:?}",
            reply.status, reply.body
        );
        if reply.string("JobStatus").as_deref() == Some("SUCCEEDED") {
            return reply;
        }
        assert_eq!(
            reply.string("JobStatus").as_deref(),
            Some("IN_PROGRESS"),
            "case {case:?}: a job is reported running until it is finished, got {:?}",
            reply.body
        );
        assert!(
            reply.field("Labels").is_none(),
            "case {case:?}: an analysis that is still running reports no results, got {:?}",
            reply.body
        );
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the job must reach SUCCEEDED, last answer {:?}",
            reply.body
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// One row of the reporting table: the video the job runs on, the members the start carries and
/// the choices the read makes.
struct ReportCase {
    case: &'static str,
    key: &'static str,
    job_tag: Option<&'static str>,
    sort_by: Option<&'static str>,
    aggregate_by: Option<&'static str>,
    max_results: Option<i64>,
}

/// A job a `StartLabelDetection` call recorded is reported under the identifier that call
/// answered with: its status as the clock owes it, the video and the tag it was started with,
/// the choices the read made, and — once it is finished — its results.
#[tokio::test]
async fn a_started_label_detection_job_is_reported_until_it_is_finished() {
    let router = seeded().await;
    let cases = [
        ReportCase {
            case: "no optional member on either call",
            key: KEYS[0],
            job_tag: None,
            sort_by: None,
            aggregate_by: None,
            max_results: None,
        },
        ReportCase {
            case: "tagged, sorted by name, aggregated by segments, one result a page",
            key: KEYS[1],
            job_tag: Some("nightly-labels"),
            sort_by: Some("NAME"),
            aggregate_by: Some("SEGMENTS"),
            max_results: Some(1),
        },
        ReportCase {
            case: "tagged, both choices stated as the defaults, the largest page",
            key: KEYS[2],
            job_tag: Some("archive.labels:2026+01"),
            sort_by: Some("TIMESTAMP"),
            aggregate_by: Some("TIMESTAMPS"),
            max_results: Some(1_000),
        },
        ReportCase {
            case: "the same video again, aggregated by segments only",
            key: KEYS[0],
            job_tag: None,
            sort_by: None,
            aggregate_by: Some("SEGMENTS"),
            max_results: Some(500),
        },
        ReportCase {
            case: "sorted by name only",
            key: KEYS[1],
            job_tag: Some("sorted-by-name"),
            sort_by: Some("NAME"),
            aggregate_by: None,
            max_results: None,
        },
    ];

    // Every job is started first and read afterwards, so the jobs of the whole table run their
    // span together rather than one after another.
    let mut started = Vec::new();
    for case in &cases {
        let mut start = json!({ "Video": staged_video(case.key) });
        if let Some(tag) = case.job_tag {
            start["JobTag"] = json!(tag);
        }
        let reply = start_labels(&router, start).await;
        started.push(job_id_of(case.case, &reply));
    }

    for (case, job_id) in cases.iter().zip(&started) {
        let mut read = json!({ "JobId": job_id });
        if let Some(sort_by) = case.sort_by {
            read["SortBy"] = json!(sort_by);
        }
        if let Some(aggregate_by) = case.aggregate_by {
            read["AggregateBy"] = json!(aggregate_by);
        }
        if let Some(max_results) = case.max_results {
            read["MaxResults"] = json!(max_results);
        }

        let finished = poll_until_succeeded(&router, case.case, &read).await;

        assert_eq!(
            finished.string("JobId").as_deref(),
            Some(job_id.as_str()),
            "case {:?}: a job is reported under the identifier its start answered with, got {:?}",
            case.case,
            finished.body
        );
        assert_eq!(
            finished.field("Video"),
            Some(&staged_video(case.key)),
            "case {:?}: expected the video the job runs on, got {:?}",
            case.case,
            finished.field("Video")
        );
        assert_eq!(
            finished.string("JobTag").as_deref(),
            case.job_tag,
            "case {:?}: expected the tag the start carried, got {:?}",
            case.case,
            finished.string("JobTag")
        );
        let metadata = finished
            .field("GetRequestMetadata")
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            metadata,
            json!({
                "SortBy": case.sort_by.unwrap_or(DEFAULT_SORT_BY),
                "AggregateBy": case.aggregate_by.unwrap_or(DEFAULT_AGGREGATE_BY),
            }),
            "case {:?}: expected the choices of the read, defaulted where it made none, got \
             {metadata:?}",
            case.case
        );

        // The results of a finished analysis: an EMPTY array, because nothing in this process
        // decodes a video, and no cursor into results there are none of.
        let labels = finished.list("Labels");
        assert!(
            finished.field("Labels").is_some() && labels.is_empty(),
            "case {:?}: a finished analysis reports an empty label array, got {:?}",
            case.case,
            finished.field("Labels")
        );
        for member in ["NextToken", "VideoMetadata", "LabelModelVersion", "StatusMessage"] {
            assert!(
                finished.field(member).is_none(),
                "case {:?}: {member} is not something this analysis measured, got {:?}",
                case.case,
                finished.field(member)
            );
        }

        // Reporting a job is a READ: the same request again answers exactly the same.
        let again = get_labels(&router, read.clone()).await;
        assert_eq!(
            again.body, finished.body,
            "case {:?}: reading a job must not change what a later caller sees, got {:?} then \
             {:?}",
            case.case, finished.body, again.body
        );
    }
}

/// An identifier naming no job of THIS analysis is refused: one this service never minted, one
/// it minted for another analysis of the family, and one that is a minted identifier changed.
#[tokio::test]
async fn an_identifier_that_names_no_label_detection_job_is_refused() {
    let router = seeded().await;
    let label_job = job_id_of(
        "the setup",
        &start_labels(&router, json!({ "Video": staged_video(KEYS[0]) })).await,
    );
    let face_job = job_id_of(
        "the setup",
        &call(
            &router,
            FACE_ANALYSIS,
            json!({ "Video": staged_video(KEYS[1]) }),
        )
        .await,
    );
    let shortened = label_job[..label_job.len() - 1].to_string();

    let cases = [
        (
            "an identifier this service never minted",
            "0123456789abcdef".to_string(),
        ),
        ("the shortest identifier the model admits", "j".to_string()),
        ("the longest identifier the model admits", "a".repeat(64)),
        ("a job of another analysis of the family", face_job),
        (
            "this analysis's own identifier with its last character dropped",
            shortened.clone(),
        ),
        (
            "this analysis's own identifier with a character changed",
            format!("{shortened}z"),
        ),
    ];

    for (case, job_id) in &cases {
        let reply = get_labels(&router, json!({ "JobId": job_id })).await;
        assert_refusal(
            case,
            &reply,
            "ResourceNotFoundException",
            &format!("The job id: {job_id} does not exist"),
        );
    }

    // The job this analysis DID start is still reported, so the refusals above are about the
    // identifier and not about the store.
    let reply = get_labels(&router, json!({ "JobId": &label_job })).await;
    assert_eq!(
        reply.status, 200,
        "the job this analysis started is still reported, got {} with {:?}",
        reply.status, reply.body
    );
}

/// A `NextToken` no page of this analysis minted names no position in its results, whatever it
/// looks like; a request that carries none, or carries the empty token the model admits, asks
/// for the first page.
#[tokio::test]
async fn a_pagination_token_this_analysis_did_not_mint_is_refused() {
    let router = seeded().await;
    let job_id = job_id_of(
        "the setup",
        &start_labels(&router, json!({ "Video": staged_video(KEYS[0]) })).await,
    );

    let refused = [
        ("a string that is not a cursor at all", "not-a-cursor".to_string()),
        ("hex that decodes to no scope", ::hex::encode("0")),
        (
            "a cursor of another paginated list of this service",
            state::encode_cursor(state::FACES_CURSOR_SCOPE, "0"),
        ),
        (
            "a cursor of another analysis of the family",
            state::encode_cursor(FACE_ANALYSIS, "0"),
        ),
        (
            "a cursor of this analysis naming no position",
            state::encode_cursor(LABEL_ANALYSIS, "not-a-position"),
        ),
    ];
    for (case, token) in &refused {
        let reply = get_labels(&router, json!({ "JobId": &job_id, "NextToken": token })).await;
        assert_refusal(
            case,
            &reply,
            "InvalidPaginationTokenException",
            INVALID_PAGINATION_TOKEN_MESSAGE,
        );
    }

    for (case, body) in [
        ("no token at all", json!({ "JobId": &job_id })),
        (
            "the empty token the model admits",
            json!({ "JobId": &job_id, "NextToken": "" }),
        ),
    ] {
        let reply = get_labels(&router, body).await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: asking for the first page must answer, got {} with {:?}",
            reply.status, reply.body
        );
    }
}

/// A label-detection job as the store holds it, started at `started_at` on a staged video.
fn label_job(job_id: &str, analysis: &str, started_at: SystemTime) -> VideoJob {
    VideoJob::new(
        job_id.to_string(),
        analysis,
        VideoLocation {
            bucket: BUCKET.to_string(),
            name: KEYS[0].to_string(),
            version: None,
        },
        started_at,
    )
}

/// The status a report answers is the clock's answer at the instant it is handed, and it is the
/// same answer however often the job is read: the job the report resolved is the one its own
/// analysis started, and reporting it writes nothing.
#[tokio::test]
async fn a_report_answers_the_status_the_clock_owes_the_job() {
    let started_at = UNIX_EPOCH + Duration::from_secs(1_767_225_600);
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let store = state::service_state(&*engine).expect("the service's own store on this engine");
    store
        .lock()
        .video_jobs
        .record(label_job("label-job", LABEL_ANALYSIS, started_at));
    store
        .lock()
        .video_jobs
        .record(label_job("face-job", FACE_ANALYSIS, started_at));

    let cases: [(&str, &str, SystemTime, Result<&str, String>); 7] = [
        (
            "at the instant it started",
            "label-job",
            started_at,
            Ok("IN_PROGRESS"),
        ),
        (
            "a moment before it is due",
            "label-job",
            started_at + COMPLETION_SPAN - Duration::from_millis(1),
            Ok("IN_PROGRESS"),
        ),
        (
            "at the instant it is due",
            "label-job",
            started_at + COMPLETION_SPAN,
            Ok("SUCCEEDED"),
        ),
        (
            "long after it is due",
            "label-job",
            started_at + COMPLETION_SPAN + Duration::from_secs(86_400),
            Ok("SUCCEEDED"),
        ),
        (
            "back at the instant it started, after it was read as finished",
            "label-job",
            started_at,
            Ok("IN_PROGRESS"),
        ),
        (
            "a job of another analysis of the family",
            "face-job",
            started_at,
            Err("The job id: face-job does not exist".to_string()),
        ),
        (
            "an identifier the store holds no job under",
            "no-such-job",
            started_at,
            Err("The job id: no-such-job does not exist".to_string()),
        ),
    ];

    for (case, job_id, now, expected) in cases {
        let reported = video_report::report(&store, LABEL_ANALYSIS, job_id, now);
        match (&expected, &reported) {
            (Ok(status), Ok(answer)) => {
                assert_eq!(
                    answer.status.modeled(),
                    *status,
                    "case {case:?}: expected {status:?}, got {:?}",
                    answer.status.modeled()
                );
                assert_eq!(
                    answer.job.job_id, job_id,
                    "case {case:?}: expected the job {job_id:?}, got {:?}",
                    answer.job.job_id
                );
            }
            (Err(message), Err(reason)) => {
                assert_eq!(
                    reason.message(),
                    message.as_str(),
                    "case {case:?}: expected the message {message:?}, got {:?}",
                    reason.message()
                );
                assert!(
                    matches!(reason, NotReported::UnknownJob(_)),
                    "case {case:?}: expected an unknown job, got {reason:?}"
                );
            }
            _ => panic!("case {case:?}: expected {expected:?}, got {reported:?}"),
        }
    }

    // Reads only: the two jobs the test recorded are still the two jobs the store holds, each
    // as it was recorded.
    let held = store.lock();
    for job_id in ["label-job", "face-job"] {
        let job = held.video_jobs.get(job_id);
        assert_eq!(
            job.map(|job| job.started_at),
            Some(started_at),
            "case {job_id:?}: reporting a job must not change the record, got {job:?}"
        );
    }
}

/// One row of the pagination table: how many results there are to serve and how large a page
/// the caller asked for.
struct PageCase {
    case: &'static str,
    results: usize,
    max_results: Option<i32>,
    /// The largest page the request can be served: what it asked for, held at the documented
    /// ceiling.
    page_size: usize,
}

/// Every result is reported on exactly one page, in order, and the last page carries no cursor:
/// a caller that follows the cursors of a `Get…` call sees each result once and stops.
#[tokio::test]
async fn every_result_is_reported_on_exactly_one_page() {
    let cases = [
        PageCase {
            case: "no result at all, no page size asked for",
            results: 0,
            max_results: None,
            page_size: MAX_RESULTS_PAGE_SIZE,
        },
        PageCase {
            case: "one result, one to a page",
            results: 1,
            max_results: Some(1),
            page_size: 1,
        },
        PageCase {
            case: "seven results, one to a page",
            results: 7,
            max_results: Some(1),
            page_size: 1,
        },
        PageCase {
            case: "seven results, three to a page",
            results: 7,
            max_results: Some(3),
            page_size: 3,
        },
        PageCase {
            case: "seven results, the documented ceiling to a page",
            results: 7,
            max_results: Some(i32::try_from(MAX_RESULTS_PAGE_SIZE).expect("the ceiling is small")),
            page_size: MAX_RESULTS_PAGE_SIZE,
        },
        PageCase {
            case: "one result past the ceiling, a page size past the ceiling",
            results: MAX_RESULTS_PAGE_SIZE + 1,
            max_results: Some(5_000),
            page_size: MAX_RESULTS_PAGE_SIZE,
        },
    ];

    for case in &cases {
        let results: Vec<u32> = (0..case.results)
            .map(|index| u32::try_from(index).expect("the case is small"))
            .collect();
        let mut reported: Vec<u32> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0usize;
        loop {
            let page = video_report::results_page(
                LABEL_ANALYSIS,
                &results,
                case.max_results,
                token.as_deref(),
            )
            .unwrap_or_else(|reason| {
                panic!("case {:?}: a cursor this analysis minted must be served, got {reason:?}",
                    case.case)
            });
            assert!(
                page.results.len() <= case.page_size,
                "case {:?}: a page serves at most {} results, got {}",
                case.case,
                case.page_size,
                page.results.len()
            );
            reported.extend(page.results.iter().copied());
            pages += 1;
            match page.next_token {
                Some(next) => token = Some(next),
                None => break,
            }
            assert!(
                pages <= results.len() + 1,
                "case {:?}: following the cursors must end, it served {pages} pages",
                case.case
            );
        }
        assert_eq!(
            reported, results,
            "case {:?}: expected every result once in order, got {reported:?}",
            case.case
        );
        let expected_pages = results.len().div_ceil(case.page_size).max(1);
        assert_eq!(
            pages, expected_pages,
            "case {:?}: expected {expected_pages} page(s), got {pages}",
            case.case
        );
    }

    // A cursor is scoped to the analysis that minted it, so one `Get…` operation's token names
    // no position in another's results.
    let results: Vec<u32> = (0..4).collect();
    let first = video_report::results_page(LABEL_ANALYSIS, &results, Some(2), None)
        .expect("the first page is served");
    let token = first
        .next_token
        .expect("four results two to a page leave a further page");
    let replayed = video_report::results_page(FACE_ANALYSIS, &results, Some(2), Some(&token));
    assert!(
        matches!(replayed, Err(NotReported::InvalidPaginationToken(_))),
        "a cursor of one analysis names no position in another's results, got {replayed:?}"
    );
}
