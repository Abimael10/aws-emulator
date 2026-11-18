// SPDX-License-Identifier: CC0-1.0

//! End-to-end coverage of STARTING a stored-video analysis over the wire (`awsJson1_1`,
//! `POST /` with `X-Amz-Target: RekognitionService.Start…`), dispatched through the same
//! [`MultiServiceRouter`] the production emulator mounts, with this service and the
//! emulator's own Amazon S3 on ONE engine — the video a start operation analyses is always an
//! object of the object store (`smithy_query(query="shape",shape="Video")` models `S3Object`
//! as its only member).
//!
//! Both members of that family this emulator implements are exercised here, because the rule
//! below is ONE rule and they share one implementation of it: `StartFaceDetection` in the
//! first half, and `StartLabelDetection` — with the members that configure a LABEL analysis,
//! its confidence floor, its features and its label filters — in the second.
//!
//! THE RULE UNDER TEST: a `Start…` operation of this API records a stored-video analysis
//! JOB. It establishes that the object store serves the video the request names, mints a
//! `JobId` for the job, persists the request under it and answers with that identifier; a
//! `ClientRequestToken` makes the start idempotent, so the same token with the same request
//! answers with the SAME job and the same token with a different request is refused; and a
//! video it cannot reach is refused with the modelled error for the reason — an object the
//! store does not serve is an unreadable S3 object, a request that identifies no object at
//! all is an invalid parameter.
//!
//! Each test asserts that rule over a TABLE whose rows differ along what the rule
//! generalises over: which object the video names (bucket, key, prefix depth, extension,
//! version), which optional members the start carries and at what values
//! (`FaceAttributes`, `JobTag`, `NotificationChannel`), which member a reused token differs
//! in, and how an unreachable video is unreachable. The recorded case the issue was filed
//! from — a start on an S3 object nothing was staged at — is one row of the refusal table.
//!
//! Nothing of the media is ever read, so the staged objects are opaque bytes: a start
//! establishes that the object is SERVED and reports nothing about what is in it.
//!
//! The last test is about the one thing the clock owes a job and no wire read reports yet
//! (`GetFaceDetection` is a scaffold stub of its own, because reporting the FACES an
//! analysis found needs a video decoder this process does not have): it drives the job
//! record's own status rule at instants it is handed, so it is decided by arithmetic and
//! never by how fast the host runs.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use service_com_amazonaws_rekognition_rekognitionservice::state::video_jobs::{
    JobStatus, VideoJob, VideoJobs, COMPLETION_SPAN, MAX_CONCURRENT_JOBS,
};
use service_com_amazonaws_rekognition_rekognitionservice::state::video_source::VideoLocation;
use smithy_server_runtime::MultiServiceRouter;

// The router harness with the object store mounted, the dispatch helpers and the reduced
// reply all come from the suites that already own them rather than being built again here.
use crate::face_collections::{call, Reply};
use crate::face_detection::{harness_with_object_store, harness_with_object_store_on_engine, s3};

/// The bucket the staged videos live in, and one the store never holds.
const BUCKET: &str = "rekognition-videos";
const ABSENT_BUCKET: &str = "rekognition-videos-missing";

/// The keys this suite stages a video at, one per container format `Video` documents as
/// supported ("the supported file formats are .mp4, .mov and .avi",
/// `smithy_query(query="shape",shape="Video")`), with one under a key prefix because a key
/// is a key whatever it looks like.
const STAGED_KEYS: &[&str] = &[
    "clip.mp4",
    "clip.mov",
    "clip.avi",
    "recordings/2026/01/clip.mp4",
];

/// The wording AWS answers an object it could not read the metadata of with. Fixed text for
/// the class: no part of it comes from the request or from this emulator's state.
const UNREADABLE_OBJECT_MESSAGE: &str =
    "Unable to get object metadata from S3. Check object key, region and/or access permissions.";

/// The wording AWS answers an `InvalidParameterException` with. Fixed text for the class.
const INVALID_PARAMETERS_MESSAGE: &str = "Request has invalid parameters";

/// The wording a reused `ClientRequestToken` that names a different request is refused with.
const IDEMPOTENT_MISMATCH_MESSAGE: &str = "Service Unavailable";

/// A notification channel whose two required members satisfy the patterns the model puts on
/// them (`smithy_query(query="shape",shape="NotificationChannel")`).
fn channel(topic: &str) -> Value {
    json!({
        "SNSTopicArn": format!("arn:aws:sns:us-east-1:{}:{topic}", engine::emulator_account_id()),
        "RoleArn": format!(
            "arn:aws:iam::{}:role/AmazonRekognitionVideoRole",
            engine::emulator_account_id()
        ),
    })
}

/// The emulator with the videos of [`STAGED_KEYS`] staged in [`BUCKET`].
///
/// The bytes are opaque: nothing in this process decodes video, so what a start operation
/// can establish about them is that the store serves the object.
async fn seeded() -> MultiServiceRouter {
    let router = harness_with_object_store();
    stage_videos(&router).await;
    router
}

/// The same emulator, with the engine it is mounted on handed back beside it: what a start
/// PERSISTED is read back from the job store itself, because the operations that report a
/// job's results are scaffold stubs of their own.
async fn seeded_on_engine() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let (router, engine) = harness_with_object_store_on_engine();
    stage_videos(&router).await;
    (router, engine)
}

/// Stage [`STAGED_KEYS`] in [`BUCKET`] through the emulator's own Amazon S3.
async fn stage_videos(router: &MultiServiceRouter) {
    assert_eq!(
        s3(router, &format!("/{BUCKET}"), &[]).await,
        200,
        "the video bucket must be created first"
    );
    for key in STAGED_KEYS {
        assert_eq!(
            s3(router, &format!("/{BUCKET}/{key}"), b"stored-video-object").await,
            200,
            "the video object {key:?} must be stored"
        );
    }
}

/// The job this service's store holds under `job_id`, read from the engine the emulator is
/// mounted on — the RECORD a start wrote, not the answer it gave.
fn recorded_job(engine: &engine::InMemoryEngine, case: &str, job_id: &str) -> VideoJob {
    let state = service_com_amazonaws_rekognition_rekognitionservice::state::service_state(engine)
        .expect("the service's own store is reachable from the engine it is mounted on");
    let held = state.lock();
    held.video_jobs
        .get(job_id)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no job was recorded under {job_id:?}"))
}

/// `Video` naming the staged object `key`.
fn staged_video(key: &str) -> Value {
    json!({ "S3Object": { "Bucket": BUCKET, "Name": key } })
}

async fn start(router: &MultiServiceRouter, body: Value) -> Reply {
    call(router, "StartFaceDetection", body).await
}

/// The `JobId` of a start that was carried out, with the constraints the model puts on a
/// `JobId` already checked (`smithy_query(query="shape",shape="JobId")`: `length 1..64`,
/// `pattern ^[a-zA-Z0-9-_]+$`).
fn job_id_of(case: &str, reply: &Reply) -> String {
    assert_eq!(
        reply.status, 200,
        "case {case:?}: the start must be carried out, got {} with {:?}",
        reply.status, reply.body
    );
    let job_id = reply.string("JobId").unwrap_or_else(|| {
        panic!("case {case:?}: a start answers with a JobId, got {:?}", reply.body)
    });
    assert!(
        (1..=64).contains(&job_id.chars().count()),
        "case {case:?}: a JobId is 1..64 characters, got {job_id:?}"
    );
    assert!(
        job_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric()
                || character == '-'
                || character == '_'),
        "case {case:?}: a JobId matches ^[a-zA-Z0-9-_]+$, got {job_id:?}"
    );
    job_id
}

/// What every refusal of a start must carry: the status, the envelope's `__type`, the body's
/// own `Code` and the class's fixed wording.
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
        "case {case:?}: expected Code {code}, got {:?}",
        reply.body
    );
    assert_eq!(
        reply.string("Message").as_deref(),
        Some(message),
        "case {case:?}: expected Message {message:?}, got {:?}",
        reply.body
    );
    assert!(
        reply.field("JobId").is_none(),
        "case {case:?}: a refused start hands out no job identifier, got {:?}",
        reply.body
    );
}

/// A start on a video the object store serves is carried out whatever else the request
/// carries, and every job it records gets an identifier of its own.
#[tokio::test]
async fn a_start_on_a_served_video_records_a_job_and_answers_its_identifier() {
    let router = seeded().await;

    // The rows differ along the two things a start varies in: which object the video names,
    // and which of the operation's optional members the request carries, at which values
    // (`smithy_query(query="operation",operation="StartFaceDetection")`:
    // `FaceAttributes` is one of DEFAULT | ALL, `JobTag` and `NotificationChannel` are free).
    let mut extras: Vec<(&str, Value)> = vec![
        ("nothing else", json!({})),
        (
            "the default attribute set",
            json!({ "FaceAttributes": "DEFAULT" }),
        ),
        (
            "every optional member at once, with a job tag of every character class the \
             pattern admits",
            json!({
                "FaceAttributes": "ALL",
                "JobTag": "group_1.a-b:c+d=e/f",
                "NotificationChannel": channel("AmazonRekognitionAll"),
            }),
        ),
    ];
    // A start carrying no token is a fresh job every time, so the same request twice is two
    // jobs rather than one: the row is repeated to assert exactly that.
    extras.push(("nothing else", json!({})));

    // Every job this table starts is still running while the table runs, so the table stays
    // below the ceiling on concurrently running jobs — which is what the ceiling's own test
    // below drives, at instants rather than over the wire.
    assert!(
        STAGED_KEYS.len() * extras.len() < MAX_CONCURRENT_JOBS,
        "the table must stay below the {MAX_CONCURRENT_JOBS} running jobs a start is \
         refused past, got {} rows",
        STAGED_KEYS.len() * extras.len()
    );

    let mut handed_out: Vec<String> = Vec::new();
    for key in STAGED_KEYS {
        for (extra_case, extra) in &extras {
            let mut body = extra.clone();
            body.as_object_mut()
                .expect("the extra members are an object")
                .insert("Video".to_string(), staged_video(key));
            let case = format!("{key} with {extra_case}");
            let job_id = job_id_of(&case, &start(&router, body).await);
            assert!(
                !handed_out.contains(&job_id),
                "case {case:?}: every start with no token records its own job, but \
                 {job_id:?} was handed out before"
            );
            handed_out.push(job_id);
        }
    }
    assert_eq!(
        handed_out.len(),
        STAGED_KEYS.len() * extras.len(),
        "every row must have been answered with a job identifier, got {handed_out:?}"
    );
}

/// A `ClientRequestToken` makes a start idempotent: "if you use the same token with multiple
/// StartFaceDetection requests, the same JobId is returned", and a token reused for a
/// request that differs in any other member is the modelled refusal — which records nothing,
/// so the job the token already started is still the one it answers with afterwards.
#[tokio::test]
async fn a_reused_client_request_token_answers_with_the_job_it_already_started() {
    let router = seeded().await;
    let token = "start-face-detection-token";
    let request = json!({
        "Video": staged_video("clip.mp4"),
        "ClientRequestToken": token,
        "FaceAttributes": "ALL",
        "JobTag": "idempotent",
        "NotificationChannel": channel("AmazonRekognitionIdempotent"),
    });

    let first = job_id_of("the first start", &start(&router, request.clone()).await);

    // Another start in between records its own job; the token still names the first one.
    let other = job_id_of(
        "an unrelated start in between",
        &start(&router, json!({ "Video": staged_video("clip.mov") })).await,
    );
    assert_ne!(
        other, first,
        "a start under no token records its own job, got {other:?} for both"
    );

    for attempt in ["the second start", "the third start"] {
        let repeated = job_id_of(attempt, &start(&router, request.clone()).await);
        assert_eq!(
            repeated, first,
            "case {attempt:?}: the same token with the same request answers with the same \
             job, expected {first:?}, got {repeated:?}"
        );
    }

    // A fresh token is a fresh request, whatever else it repeats.
    let fresh = json!({
        "Video": staged_video("clip.mp4"),
        "ClientRequestToken": "start-face-detection-token-2",
        "FaceAttributes": "ALL",
        "JobTag": "idempotent",
        "NotificationChannel": channel("AmazonRekognitionIdempotent"),
    });
    let under_fresh_token = job_id_of("a fresh token", &start(&router, fresh).await);
    assert_ne!(
        under_fresh_token, first,
        "a different token starts a different job, got {first:?} for both"
    );

    // One row per member "at least one of the other input parameters" can differ in: the
    // video's bucket, key and version, the analysis's own parameter, the job tag and the
    // notification channel, each differing in exactly one member from the committed request.
    let mismatches: Vec<(&str, Value)> = vec![
        (
            "a different object key",
            json!({
                "Video": staged_video("clip.mov"),
                "ClientRequestToken": token,
                "FaceAttributes": "ALL",
                "JobTag": "idempotent",
                "NotificationChannel": channel("AmazonRekognitionIdempotent"),
            }),
        ),
        (
            "a pinned object version",
            json!({
                "Video": { "S3Object": { "Bucket": BUCKET, "Name": "clip.mp4", "Version": "0123456789abcdef" } },
                "ClientRequestToken": token,
                "FaceAttributes": "ALL",
                "JobTag": "idempotent",
                "NotificationChannel": channel("AmazonRekognitionIdempotent"),
            }),
        ),
        (
            "a different attribute set",
            json!({
                "Video": staged_video("clip.mp4"),
                "ClientRequestToken": token,
                "FaceAttributes": "DEFAULT",
                "JobTag": "idempotent",
                "NotificationChannel": channel("AmazonRekognitionIdempotent"),
            }),
        ),
        (
            "no attribute set at all",
            json!({
                "Video": staged_video("clip.mp4"),
                "ClientRequestToken": token,
                "JobTag": "idempotent",
                "NotificationChannel": channel("AmazonRekognitionIdempotent"),
            }),
        ),
        (
            "a different job tag",
            json!({
                "Video": staged_video("clip.mp4"),
                "ClientRequestToken": token,
                "FaceAttributes": "ALL",
                "JobTag": "different",
                "NotificationChannel": channel("AmazonRekognitionIdempotent"),
            }),
        ),
        (
            "a different notification topic",
            json!({
                "Video": staged_video("clip.mp4"),
                "ClientRequestToken": token,
                "FaceAttributes": "ALL",
                "JobTag": "idempotent",
                "NotificationChannel": channel("AmazonRekognitionOther"),
            }),
        ),
        (
            "no notification channel",
            json!({
                "Video": staged_video("clip.mp4"),
                "ClientRequestToken": token,
                "FaceAttributes": "ALL",
                "JobTag": "idempotent",
            }),
        ),
    ];
    for (case, body) in mismatches {
        let reply = start(&router, body).await;
        assert_refusal(
            case,
            &reply,
            "IdempotentParameterMismatchException",
            IDEMPOTENT_MISMATCH_MESSAGE,
        );
        let still = job_id_of("the token after a refused reuse", &start(&router, request.clone()).await);
        assert_eq!(
            still, first,
            "case {case:?}: a refused reuse records nothing, so the token still names \
             {first:?}, got {still:?}"
        );
    }
}

/// A video the object store does not serve is refused as an unreadable S3 object, however it
/// fails to name one the store holds.
#[tokio::test]
async fn a_video_the_store_does_not_serve_is_refused_as_an_unreadable_object() {
    let router = seeded().await;
    let unreachable: Vec<(&str, Value)> = vec![
        // The recorded case this issue was filed from: a start on an object nothing staged.
        (
            "a bucket the store does not hold",
            json!({ "S3Object": { "Bucket": ABSENT_BUCKET, "Name": "clip.mp4" } }),
        ),
        (
            "a key the bucket does not hold",
            json!({ "S3Object": { "Bucket": BUCKET, "Name": "absent.mp4" } }),
        ),
        (
            "a key that only prefixes a stored one",
            json!({ "S3Object": { "Bucket": BUCKET, "Name": "clip" } }),
        ),
        (
            "a key that only extends a stored one",
            json!({ "S3Object": { "Bucket": BUCKET, "Name": "clip.mp4.part" } }),
        ),
        (
            "a version the bucket never held",
            json!({ "S3Object": { "Bucket": BUCKET, "Name": "clip.mp4", "Version": "0123456789abcdef" } }),
        ),
        // An object named incompletely is an object nothing could be read from: there is no
        // address to read without both a bucket and a key.
        (
            "a bucket with no key",
            json!({ "S3Object": { "Bucket": BUCKET } }),
        ),
        (
            "a key with no bucket",
            json!({ "S3Object": { "Name": "clip.mp4" } }),
        ),
        ("an object naming neither", json!({ "S3Object": {} })),
    ];
    for (case, video) in unreachable {
        let reply = start(&router, json!({ "Video": video })).await;
        assert_refusal(
            case,
            &reply,
            "InvalidS3ObjectException",
            UNREADABLE_OBJECT_MESSAGE,
        );
    }
}

/// A request whose `Video` identifies no object at all is refused as the invalid parameter it
/// is — never as an unreadable object, so the two failures stay distinguishable — whatever
/// else the request carries.
#[tokio::test]
async fn a_request_that_identifies_no_video_is_refused_as_an_invalid_parameter() {
    let router = seeded().await;
    let extras: Vec<(&str, Value)> = vec![
        ("nothing else", json!({})),
        ("an attribute set", json!({ "FaceAttributes": "ALL" })),
        ("a job tag", json!({ "JobTag": "no-video" })),
        (
            "a token and a notification channel",
            json!({
                "ClientRequestToken": "no-video-token",
                "NotificationChannel": channel("AmazonRekognitionNoVideo"),
            }),
        ),
    ];
    for (case, extra) in extras {
        let mut body = extra.clone();
        body.as_object_mut()
            .expect("the extra members are an object")
            .insert("Video".to_string(), json!({}));
        let reply = start(&router, body).await;
        assert_refusal(
            case,
            &reply,
            "InvalidParameterException",
            INVALID_PARAMETERS_MESSAGE,
        );
    }
}

/// One job record, started at `started_at` on a video of the staged bucket.
fn job(job_id: &str, started_at: SystemTime) -> VideoJob {
    VideoJob::new(
        job_id.to_string(),
        "StartFaceDetection",
        VideoLocation {
            bucket: BUCKET.to_string(),
            name: "clip.mp4".to_string(),
            version: None,
        },
        started_at,
    )
}

/// A recorded job is running until the instant the clock owes its completion, and finished
/// from that instant on — the transition the start operation's own answer leaves pending.
/// The instants are handed to the record, so the rule is decided by arithmetic and nothing
/// here waits, sleeps or reads the host's clock.
#[tokio::test]
async fn a_job_is_running_until_the_clock_reaches_its_completion() {
    let started_at = UNIX_EPOCH + Duration::from_secs(1_767_225_600);
    let recorded = job("job-under-test", started_at);

    let cases: Vec<(&str, SystemTime, JobStatus, &str)> = vec![
        ("the instant it started", started_at, JobStatus::InProgress, "IN_PROGRESS"),
        (
            "a nanosecond in",
            started_at + Duration::from_nanos(1),
            JobStatus::InProgress,
            "IN_PROGRESS",
        ),
        (
            "a nanosecond before it is due",
            started_at + COMPLETION_SPAN - Duration::from_nanos(1),
            JobStatus::InProgress,
            "IN_PROGRESS",
        ),
        (
            "the instant it is due",
            started_at + COMPLETION_SPAN,
            JobStatus::Succeeded,
            "SUCCEEDED",
        ),
        (
            "long after it is due",
            started_at + COMPLETION_SPAN + Duration::from_secs(3_600),
            JobStatus::Succeeded,
            "SUCCEEDED",
        ),
    ];
    for (case, now, expected, modeled) in cases {
        let actual = recorded.status_at(now);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
        assert_eq!(
            actual.modeled(),
            modeled,
            "case {case:?}: expected the modelled value {modeled:?}, got {:?}",
            actual.modeled()
        );
    }
    assert!(
        COMPLETION_SPAN > Duration::ZERO,
        "a start answers while its analysis is still running, so the span is never zero"
    );
}

/// The store counts as running exactly the jobs whose completion the clock has not reached,
/// so the concurrency ceiling is reached by jobs that are still running and released by the
/// same jobs once they are due.
#[tokio::test]
async fn the_concurrency_ceiling_counts_only_the_jobs_still_running() {
    let started_at = UNIX_EPOCH + Duration::from_secs(1_767_225_600);
    let due = started_at + COMPLETION_SPAN;
    let mut jobs = VideoJobs::default();

    for started in 0..MAX_CONCURRENT_JOBS {
        let job_id = format!("job-{started}");
        jobs.record(job(&job_id, started_at));
        let running = jobs.running_at(started_at);
        assert_eq!(
            running,
            started + 1,
            "case {job_id:?}: every recorded job is running at the instant it started, \
             expected {}, got {running}",
            started + 1
        );
        assert_eq!(
            jobs.get(&job_id).map(|job| job.job_id.as_str()),
            Some(job_id.as_str()),
            "case {job_id:?}: a recorded job is the one its identifier names, got {:?}",
            jobs.get(&job_id).map(|job| job.job_id.clone())
        );
        let at_limit = jobs.at_concurrency_limit(started_at);
        assert_eq!(
            at_limit,
            started + 1 >= MAX_CONCURRENT_JOBS,
            "case {job_id:?}: the ceiling is {MAX_CONCURRENT_JOBS} running jobs, with \
             {} recorded it reported {at_limit}",
            started + 1
        );
        // Due is due whatever else is running: a job that is finished is not running.
        assert_eq!(
            jobs.running_at(due),
            0,
            "case {job_id:?}: no job is still running once the clock has reached its \
             completion, got {}",
            jobs.running_at(due)
        );
        assert!(
            !jobs.at_concurrency_limit(due),
            "case {job_id:?}: a ceiling of running jobs is released by their completion"
        );
    }

    // The token lookup is per analysis AND per token: a job records the token it was started
    // under, and nothing else answers for it.
    let mut tokened = job("job-with-token", started_at);
    tokened.client_request_token = Some("a-token".to_string());
    jobs.record(tokened);
    for (case, analysis, token, expected) in [
        (
            "the analysis and token it was started under",
            "StartFaceDetection",
            "a-token",
            Some("job-with-token"),
        ),
        (
            "another token of the same analysis",
            "StartFaceDetection",
            "another-token",
            None,
        ),
        (
            "the same token of another analysis",
            "StartLabelDetection",
            "a-token",
            None,
        ),
    ] {
        let found = jobs
            .started_under_token(analysis, token)
            .map(|job| job.job_id.clone());
        assert_eq!(
            found.as_deref(),
            expected,
            "case {case:?}: expected {expected:?}, got {found:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// `StartLabelDetection`: the same rule, with the members that configure a LABEL analysis
// ---------------------------------------------------------------------------

/// The analysis a label-detection job records itself under.
const LABEL_ANALYSIS: &str = "StartLabelDetection";

/// The one value `LabelDetectionFeatureName` admits
/// (`smithy_query(query="shape",shape="LabelDetectionFeatureName")`).
const GENERAL_LABELS: &str = "GENERAL_LABELS";

async fn start_labels(router: &MultiServiceRouter, body: Value) -> Reply {
    call(router, LABEL_ANALYSIS, body).await
}

/// `Settings` narrowing the analysis with the four filter lists `GeneralLabelsSettings`
/// models (`smithy_query(query="shape",shape="GeneralLabelsSettings")`).
fn settings(
    include: &[&str],
    exclude: &[&str],
    include_categories: &[&str],
    exclude_categories: &[&str],
) -> Value {
    json!({
        "GeneralLabels": {
            "LabelInclusionFilters": include,
            "LabelExclusionFilters": exclude,
            "LabelCategoryInclusionFilters": include_categories,
            "LabelCategoryExclusionFilters": exclude_categories,
        }
    })
}

/// One row of the label-detection start table: what the case is, the members the request
/// carries beside the video, and the members the job must then hold for this analysis.
struct LabelStartCase {
    case: &'static str,
    extra: Value,
    recorded: Vec<(&'static str, &'static str)>,
}

/// A start carries out a label detection on a video the object store serves, and the job it
/// records holds the request: the analysis that started it, the video, the tag and channel it
/// carries and the members that configure the LABEL analysis itself, each as the request
/// stated it and none of them as the value the analysis would have applied.
///
/// The rows differ along what the rule generalises over: which object the video names, and
/// which of this operation's own optional members the request carries, at which values — the
/// confidence floor at both published bounds of `Percent` and at a fractional value between
/// them, the one feature the model admits, and each filter list of `Settings`.
#[tokio::test]
async fn a_label_detection_start_records_the_request_under_the_job_it_answers_with() {
    let (router, engine) = seeded_on_engine().await;

    let rows: Vec<LabelStartCase> = vec![
        LabelStartCase {
            case: "nothing but the video",
            extra: json!({}),
            recorded: vec![],
        },
        LabelStartCase {
            case: "the lowest confidence Percent admits",
            extra: json!({ "MinConfidence": 0.0 }),
            recorded: vec![("MinConfidence", "0")],
        },
        LabelStartCase {
            case: "the highest confidence Percent admits",
            extra: json!({ "MinConfidence": 100.0 }),
            recorded: vec![("MinConfidence", "100")],
        },
        LabelStartCase {
            case: "a fractional confidence between them",
            extra: json!({ "MinConfidence": 62.5 }),
            recorded: vec![("MinConfidence", "62.5")],
        },
        LabelStartCase {
            case: "the one feature the model admits, with a tag and a channel",
            extra: json!({
                "Features": [GENERAL_LABELS],
                "JobTag": "labels_1.a-b:c+d=e/f",
                "NotificationChannel": channel("AmazonRekognitionLabels"),
            }),
            recorded: vec![("Features.1", GENERAL_LABELS)],
        },
        LabelStartCase {
            case: "every filter list at once, beside a confidence floor",
            extra: json!({
                "MinConfidence": 80.0,
                "Features": [GENERAL_LABELS],
                "Settings": settings(
                    &["Person", "Car"],
                    &["Tree"],
                    &["Person Description"],
                    &["Food and Beverage"],
                ),
            }),
            recorded: vec![
                ("MinConfidence", "80"),
                ("Features.1", GENERAL_LABELS),
                ("Settings.GeneralLabels.LabelInclusionFilters.1", "Person"),
                ("Settings.GeneralLabels.LabelInclusionFilters.2", "Car"),
                ("Settings.GeneralLabels.LabelExclusionFilters.1", "Tree"),
                (
                    "Settings.GeneralLabels.LabelCategoryInclusionFilters.1",
                    "Person Description",
                ),
                (
                    "Settings.GeneralLabels.LabelCategoryExclusionFilters.1",
                    "Food and Beverage",
                ),
            ],
        },
    ];
    assert!(
        rows.len() < MAX_CONCURRENT_JOBS,
        "every job this table starts is still running while it runs, so the table must stay \
         below the {MAX_CONCURRENT_JOBS} running jobs a start is refused past, got {} rows",
        rows.len()
    );

    let mut handed_out: Vec<String> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let LabelStartCase {
            case,
            extra,
            recorded: expected,
        } = row;
        // Which object the video names varies with the row, because a start is a start
        // whatever key it is given.
        let key = STAGED_KEYS[index % STAGED_KEYS.len()];
        let mut body = extra.clone();
        let members = body.as_object_mut().expect("the extra members are an object");
        members.insert("Video".to_string(), staged_video(key));
        let case = format!("{case} on {key}");

        let job_id = job_id_of(&case, &start_labels(&router, body).await);
        assert!(
            !handed_out.contains(&job_id),
            "case {case:?}: every start with no token records its own job, but {job_id:?} was \
             handed out before"
        );
        handed_out.push(job_id.clone());

        let job = recorded_job(&engine, &case, &job_id);
        assert_eq!(
            job.analysis, LABEL_ANALYSIS,
            "case {case:?}: the job records the analysis that started it, got {:?}",
            job.analysis
        );
        assert_eq!(
            job.video,
            VideoLocation {
                bucket: BUCKET.to_string(),
                name: key.to_string(),
                version: None,
            },
            "case {case:?}: the job records the video the request named, got {:?}",
            job.video
        );
        assert_eq!(
            job.client_request_token, None,
            "case {case:?}: no token was sent, so the job records none, got {:?}",
            job.client_request_token
        );
        assert_eq!(
            job.job_tag.as_deref(),
            extra.get("JobTag").and_then(Value::as_str),
            "case {case:?}: the job records the tag the request carried, got {:?}",
            job.job_tag
        );
        assert_eq!(
            job.notification_channel.is_some(),
            extra.get("NotificationChannel").is_some(),
            "case {case:?}: the job records the channel exactly when the request carried one, \
             got {:?}",
            job.notification_channel
        );
        let recorded: Vec<(&str, &str)> = job
            .parameters
            .iter()
            .map(|(member, value)| (member.as_str(), value.as_str()))
            .collect();
        assert_eq!(
            recorded, *expected,
            "case {case:?}: expected the stated members {expected:?}, got {recorded:?}"
        );

        // The start answered while its analysis is still running, and the clock owes the
        // completion: the instants are handed to the record, so nothing here waits.
        assert_eq!(
            job.status_at(job.started_at),
            JobStatus::InProgress,
            "case {case:?}: a job is running at the instant it started, got {:?}",
            job.status_at(job.started_at)
        );
        assert_eq!(
            job.status_at(job.started_at + COMPLETION_SPAN),
            JobStatus::Succeeded,
            "case {case:?}: a job is finished once the clock reaches its completion, got {:?}",
            job.status_at(job.started_at + COMPLETION_SPAN)
        );
    }
}

/// A `ClientRequestToken` makes a label-detection start idempotent: "if you use the same
/// token with multiple StartLabelDetection requests, the same JobId is returned", and a token
/// reused for a request that differs in ANY other input member — including one of this
/// analysis's own — is the modelled refusal, which records nothing.
#[tokio::test]
async fn a_reused_label_detection_token_answers_with_the_job_it_already_started() {
    let router = seeded().await;
    let token = "start-label-detection-token";
    let committed = json!({
        "Video": staged_video("clip.mp4"),
        "ClientRequestToken": token,
        "MinConfidence": 50.0,
        "Features": [GENERAL_LABELS],
        "JobTag": "idempotent",
        "NotificationChannel": channel("AmazonRekognitionLabels"),
        "Settings": settings(&["Person", "Car"], &["Tree"], &["Person Description"], &[]),
    });

    let first = job_id_of("the first start", &start_labels(&router, committed.clone()).await);

    // An unrelated start in between records its own job; the token still names the first one.
    let other = job_id_of(
        "an unrelated start in between",
        &start_labels(&router, json!({ "Video": staged_video("clip.mov") })).await,
    );
    assert_ne!(
        other, first,
        "a start under no token records its own job, got {other:?} for both"
    );

    // The same request again is the same job, and a number is the number it is rather than
    // the spelling it arrived in.
    let mut as_integer_confidence = committed.clone();
    as_integer_confidence
        .as_object_mut()
        .expect("the request is an object")
        .insert("MinConfidence".to_string(), json!(50));
    for (case, body) in [
        ("the second start", committed.clone()),
        ("the third start", committed.clone()),
        ("the same confidence in integer form", as_integer_confidence),
    ] {
        let repeated = job_id_of(case, &start_labels(&router, body).await);
        assert_eq!(
            repeated, first,
            "case {case:?}: the same token with the same request answers with the same job, \
             expected {first:?}, got {repeated:?}"
        );
    }

    // One row per member "at least one of the other input parameters" can differ in: the
    // video, the tag, the channel, and every member that configures THIS analysis — the
    // confidence floor, the features, and each filter list, differing in an element, in how
    // many elements it holds, in their order and in which filter list holds them.
    let mismatches: Vec<(&str, Value)> = vec![
        ("a different object key", json!({ "Video": staged_video("clip.mov") })),
        ("a different confidence floor", json!({ "MinConfidence": 75.0 })),
        ("no confidence floor at all", json!({ "MinConfidence": null })),
        ("no features at all", json!({ "Features": null })),
        ("an empty feature list", json!({ "Features": [] })),
        (
            "a different label in the inclusion filters",
            json!({
                "Settings": settings(&["Person", "Bicycle"], &["Tree"], &["Person Description"], &[])
            }),
        ),
        (
            "one more label in the inclusion filters",
            json!({
                "Settings": settings(
                    &["Person", "Car", "Bicycle"],
                    &["Tree"],
                    &["Person Description"],
                    &[],
                )
            }),
        ),
        (
            "the same labels in the inclusion filters in another order",
            json!({
                "Settings": settings(&["Car", "Person"], &["Tree"], &["Person Description"], &[])
            }),
        ),
        (
            "the same label in the other filter list",
            json!({
                "Settings": settings(&["Person", "Car"], &[], &["Person Description"], &["Tree"])
            }),
        ),
        ("no settings at all", json!({ "Settings": null })),
        ("a different job tag", json!({ "JobTag": "different" })),
        (
            "a different notification topic",
            json!({ "NotificationChannel": channel("AmazonRekognitionOther") }),
        ),
        ("no notification channel", json!({ "NotificationChannel": null })),
    ];
    for (case, difference) in mismatches {
        let mut body = committed.clone();
        let members = body.as_object_mut().expect("the request is an object");
        for (member, value) in difference
            .as_object()
            .expect("the difference is an object")
            .iter()
        {
            if value.is_null() {
                members.remove(member);
            } else {
                members.insert(member.clone(), value.clone());
            }
        }
        let reply = start_labels(&router, body).await;
        assert_refusal(
            case,
            &reply,
            "IdempotentParameterMismatchException",
            IDEMPOTENT_MISMATCH_MESSAGE,
        );
        let still = job_id_of(
            "the token after a refused reuse",
            &start_labels(&router, committed.clone()).await,
        );
        assert_eq!(
            still, first,
            "case {case:?}: a refused reuse records nothing, so the token still names \
             {first:?}, got {still:?}"
        );
    }

    // A fresh token is a fresh request, whatever else it repeats.
    let mut fresh = committed.clone();
    fresh
        .as_object_mut()
        .expect("the request is an object")
        .insert(
            "ClientRequestToken".to_string(),
            json!("start-label-detection-token-2"),
        );
    let under_fresh_token = job_id_of("a fresh token", &start_labels(&router, fresh).await);
    assert_ne!(
        under_fresh_token, first,
        "a different token starts a different job, got {first:?} for both"
    );
}

/// A list member the request SENDS EMPTY asks the analysis for nothing, which is what leaving
/// it out asks for too, so the two state the same request: a token committed without
/// `Features` and without `Settings` answers with the same job when they arrive as empty
/// lists, and with the modelled refusal as soon as either states an element.
#[tokio::test]
async fn an_empty_list_member_states_the_same_request_as_one_left_out() {
    let router = seeded().await;
    let token = "start-label-detection-empty-lists";
    let committed = json!({
        "Video": staged_video("clip.mp4"),
        "ClientRequestToken": token,
        "MinConfidence": 50.0,
    });
    let first = job_id_of(
        "the start that left the lists out",
        &start_labels(&router, committed.clone()).await,
    );

    // Each row sends the same request with one list member present and empty.
    let empty: Vec<(&str, Value)> = vec![
        ("an empty feature list", json!({ "Features": [] })),
        (
            "empty label filters",
            json!({ "Settings": settings(&[], &[], &[], &[]) }),
        ),
        (
            "an empty feature list and empty label filters at once",
            json!({ "Features": [], "Settings": settings(&[], &[], &[], &[]) }),
        ),
    ];
    for (case, difference) in empty {
        let mut body = committed.clone();
        let members = body.as_object_mut().expect("the request is an object");
        for (member, value) in difference
            .as_object()
            .expect("the difference is an object")
            .iter()
        {
            members.insert(member.clone(), value.clone());
        }
        let repeated = job_id_of(case, &start_labels(&router, body).await);
        assert_eq!(
            repeated, first,
            "case {case:?}: an empty list states the request the committed one stated, so the \
             token answers with {first:?}, got {repeated:?}"
        );
    }

    // One element in either list is a different request, so the same token is refused.
    let stated: Vec<(&str, Value)> = vec![
        (
            "one feature",
            json!({ "Features": [GENERAL_LABELS], "Settings": settings(&[], &[], &[], &[]) }),
        ),
        (
            "one label filter",
            json!({ "Features": [], "Settings": settings(&["Person"], &[], &[], &[]) }),
        ),
    ];
    for (case, difference) in stated {
        let mut body = committed.clone();
        let members = body.as_object_mut().expect("the request is an object");
        for (member, value) in difference
            .as_object()
            .expect("the difference is an object")
            .iter()
        {
            members.insert(member.clone(), value.clone());
        }
        let reply = start_labels(&router, body).await;
        assert_refusal(
            case,
            &reply,
            "IdempotentParameterMismatchException",
            IDEMPOTENT_MISMATCH_MESSAGE,
        );
    }
}

/// A label-detection start on a video this service cannot reach is refused with the modelled
/// error for the reason, whatever else the request asks the analysis for, and hands out no
/// job identifier.
#[tokio::test]
async fn a_label_detection_start_on_an_unreachable_video_is_refused() {
    let router = seeded().await;

    // (case, the video, the modelled code, the wording of that class)
    let refusals: Vec<(&str, Value, &str, &str)> = vec![
        (
            "a bucket the store does not hold",
            json!({ "S3Object": { "Bucket": ABSENT_BUCKET, "Name": "clip.mp4" } }),
            "InvalidS3ObjectException",
            UNREADABLE_OBJECT_MESSAGE,
        ),
        (
            "a key the bucket does not hold",
            json!({ "S3Object": { "Bucket": BUCKET, "Name": "absent.mp4" } }),
            "InvalidS3ObjectException",
            UNREADABLE_OBJECT_MESSAGE,
        ),
        (
            "a version the bucket never held",
            json!({ "S3Object": { "Bucket": BUCKET, "Name": "clip.mp4", "Version": "0123456789abcdef" } }),
            "InvalidS3ObjectException",
            UNREADABLE_OBJECT_MESSAGE,
        ),
        (
            "a key with no bucket",
            json!({ "S3Object": { "Name": "clip.mp4" } }),
            "InvalidS3ObjectException",
            UNREADABLE_OBJECT_MESSAGE,
        ),
        (
            "a request that identifies no object at all",
            json!({}),
            "InvalidParameterException",
            INVALID_PARAMETERS_MESSAGE,
        ),
    ];
    // The analysis's own members do not change how an unreachable video is refused, so each
    // row is sent with and without them.
    let extras: Vec<(&str, Value)> = vec![
        ("nothing else", json!({})),
        (
            "every member that configures the analysis",
            json!({
                "MinConfidence": 55.0,
                "Features": [GENERAL_LABELS],
                "JobTag": "unreachable",
                "Settings": settings(&["Person"], &[], &[], &[]),
            }),
        ),
    ];
    for (case, video, code, message) in refusals {
        for (extra_case, extra) in &extras {
            let mut body = extra.clone();
            body.as_object_mut()
                .expect("the extra members are an object")
                .insert("Video".to_string(), video.clone());
            let case = format!("{case} with {extra_case}");
            let reply = start_labels(&router, body).await;
            assert_refusal(&case, &reply, code, message);
        }
    }
}

/// One token sent to two operations of the family commits two independent requests — the
/// modelled refusal is about reusing a token "with an operation" — while both jobs land in
/// the one job store, each under the analysis that started it.
#[tokio::test]
async fn one_token_sent_to_two_start_operations_starts_two_jobs() {
    let (router, engine) = seeded_on_engine().await;
    let token = "shared-across-operations";
    let video = staged_video("clip.mp4");

    let faces = job_id_of(
        "the face detection",
        &start(
            &router,
            json!({ "Video": video.clone(), "ClientRequestToken": token }),
        )
        .await,
    );
    let labels = job_id_of(
        "the label detection",
        &start_labels(
            &router,
            json!({ "Video": video, "ClientRequestToken": token }),
        )
        .await,
    );
    assert_ne!(
        faces, labels,
        "a token is committed per operation, so each start records its own job, got \
         {faces:?} for both"
    );

    for (case, job_id, analysis) in [
        ("the face detection", &faces, "StartFaceDetection"),
        ("the label detection", &labels, LABEL_ANALYSIS),
    ] {
        let job = recorded_job(&engine, case, job_id);
        assert_eq!(
            job.analysis, analysis,
            "case {case:?}: the job records the analysis that started it, got {:?}",
            job.analysis
        );
        assert_eq!(
            job.client_request_token.as_deref(),
            Some(token),
            "case {case:?}: the job records the token it was started under, got {:?}",
            job.client_request_token
        );
    }
}
