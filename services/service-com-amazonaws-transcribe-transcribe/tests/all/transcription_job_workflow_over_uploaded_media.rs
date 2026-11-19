// SPDX-License-Identifier: CC0-1.0

//! The documented workflow of this service, end to end over the wire and over media this
//! emulator's OWN Amazon S3 holds: upload the audio, start a job on it, poll that job to a
//! terminal status, follow the location it reports to the transcript, then delete the job.
//!
//! THE RULES THESE ROWS ASSERT, both of them about the whole of that workflow rather than
//! about one request:
//!
//! * A start whose `Media` names an object the object store ANSWERS for is ACCEPTED, and
//!   every later step of its own job then works: "To make a StartTranscriptionJob request,
//!   you must first upload your media file into an Amazon S3 bucket; you can then specify
//!   the Amazon S3 location of the file using the Media parameter"
//!   (`smithy_query(query="operation", operation="StartTranscriptionJob")`). So the start
//!   answers 200 with the job it stored — in `IN_PROGRESS`, carrying the name, language and
//!   media the request asked for and the instants the service stamped — the reader reports
//!   that same job until the clock carries it to `COMPLETED` with the location of its
//!   transcript, and the delete of a job this account holds is accepted and frees the name.
//!   An upload this emulator took and a start it refuses for a missing object are the same
//!   defect seen twice: the store is asked about the very object the upload wrote
//!   (`crate::media_object_resolution` pins the refusal half of that rule, over the
//!   addresses that do NOT resolve).
//!
//! * Every location the job reports is one a CLIENT can follow: an address on the endpoint
//!   this emulator is reached on, naming the object this emulator really stored, which
//!   serves the document when it is fetched the way the bucket's owner fetches it — with no
//!   credential of the client's own for a file in the service-managed store, whose address
//!   is pre-signed, and with the caller's own credentials for a bucket the request named.
//!   And every instant it reports carries the resolution the members document
//!   ("Timestamps are in the format `YYYY-MM-DD'T'HH:MM:SS.SSSSSS-UTC`. For example,
//!   `2022-05-04T12:32:58.761000-07:00`", `smithy_query(query="doc",
//!   shape="TranscriptionJob", member="CreationTime")` — milliseconds), so a client that
//!   round-trips one through a millisecond-resolution timestamp type reads back what it was
//!   sent.
//!
//! The rows differ along what those rules generalise over: where the audio was uploaded (a
//! key at the bucket root, a key under a nested prefix), which modelled `MediaFormat` it is
//! and whether the request declares it, and where the transcript goes (this service's own
//! store, a bucket the request named, and that bucket with an `OutputKey` naming a folder).
//! The emulator mounts Amazon S3 beside transcribe on ONE shared engine, which is what the
//! assembled router does (`crate::media_object_resolution::emulator`, reused rather than
//! stood up again), because the upload, the start's read of it and the fetch of the
//! transcript all have to meet in the same store.

use serde_json::{json, Value};
use smithy_server_runtime::{MultiServiceRouter, UNSERVED_URL_BASE};

use crate::media_object_resolution::{call, emulator, given_object, s3};
use crate::served_download_locations::{fetch, named_object};
use crate::transcription_jobs::Transcribe;

/// The bucket the audio of these rows is uploaded to, as the caller uploads it before
/// naming it.
const MEDIA_BUCKET: &str = "example-workflow-media";

/// The bucket the rows that direct their transcript to one of their own name. The caller
/// creates it, so it is created here too.
const OUTPUT_BUCKET: &str = "example-workflow-output";

/// The bytes every uploaded object holds. Nothing decodes a media file here — what the rows
/// are about is the object being THERE — so one short payload serves every row.
const AUDIO: &[u8] = b"RIFF....WAVEfmt emulator workflow audio";

/// The sentence this service refuses a read of a job it does not hold with, as
/// `crate::state::not_found_message` words it for a job: the whole fixed text, with no part
/// of the request in it.
const NO_SUCH_JOB: &str =
    "The requested job couldn't be found. Check the job name and try your request again.";

/// One row: where the audio is, what the start declares about it, and where the transcript
/// is directed.
struct Row {
    /// What this row varies, named in every assertion it fails.
    case: &'static str,
    /// The job's name, which is also the stem of its transcript's key.
    name: &'static str,
    /// The key the audio is uploaded to in [`MEDIA_BUCKET`].
    media_key: &'static str,
    /// The `MediaFormat` the start declares, or `None` for a start that declares none and
    /// leaves the format to the service's own determination.
    media_format: Option<&'static str>,
    /// The output members the start carries: none at all for the service-managed store.
    output: fn() -> Value,
    /// The key the transcript is stored under, in [`OUTPUT_BUCKET`] for a row that named it
    /// and in the service-managed bucket otherwise.
    transcript_key: fn(&str) -> String,
    /// Whether the transcript goes to this service's OWN store, whose addresses are
    /// pre-signed and fetched with no credential of the client's own.
    service_managed: bool,
}

/// The rows: every combination the two rules generalise over that changes what the workflow
/// has to do. Each `media_key` carries one of the modelled `MediaFormat` extensions
/// (`smithy_query(query="shape", shape="MediaFormat")`: amr | flac | m4a | mp3 | mp4 | ogg |
/// wav | webm), which is what the format of a stored object is read from.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "audio at the bucket root, a declared format, the service-managed store",
            name: "workflow-own-store",
            media_key: "speech.mp3",
            media_format: Some("mp3"),
            output: || json!({}),
            transcript_key: |name| {
                format!(
                    "{account}/{name}.json",
                    account = engine::emulator_account_id()
                )
            },
            service_managed: true,
        },
        Row {
            case: "audio under a nested prefix, no declared format, the caller's bucket",
            name: "workflow-callers-bucket",
            media_key: "recordings/2026/meeting.wav",
            media_format: None,
            output: || json!({"OutputBucketName": OUTPUT_BUCKET}),
            transcript_key: |name| format!("{name}.json"),
            service_managed: false,
        },
        Row {
            case: "audio in another format, an OutputKey naming a folder in the caller's \
                   bucket",
            name: "workflow-callers-folder",
            media_key: "recordings/consult.flac",
            media_format: Some("flac"),
            output: || json!({"OutputBucketName": OUTPUT_BUCKET, "OutputKey": "transcripts/"}),
            transcript_key: |name| format!("transcripts/{name}.json"),
            service_managed: false,
        },
    ]
}

/// The `Media.MediaFileUri` of a row: the address of the object the upload wrote.
fn media_uri(row: &Row) -> String {
    format!("s3://{MEDIA_BUCKET}/{}", row.media_key)
}

/// The start request of a row.
fn start_request(row: &Row) -> Value {
    let mut request = json!({
        "TranscriptionJobName": row.name,
        "LanguageCode": "en-US",
        "Media": {"MediaFileUri": media_uri(row)},
    });
    if let Some(format) = row.media_format {
        request["MediaFormat"] = json!(format);
    }
    let Value::Object(output) = (row.output)() else {
        panic!("case {:?}: a row's output members are an object", row.case);
    };
    for (member, value) in output {
        request[member] = value;
    }
    request
}

/// Upload the row's audio, as the operation's documentation requires of the caller, and read
/// it back through Amazon S3 so the row starts from an object the store really answers for.
async fn given_uploaded_audio(router: &MultiServiceRouter, row: &Row) {
    given_object(router, MEDIA_BUCKET, row.media_key, AUDIO).await;
    let (status, body) = s3(router, "GET", &format!("/{MEDIA_BUCKET}/{}", row.media_key), &[]).await;
    assert_eq!(
        status, 200,
        "case {:?}: the uploaded audio must read back before the job is started, got HTTP \
         {status} with body {:?}",
        row.case,
        String::from_utf8_lossy(&body)
    );
}

/// Follow a reported location the way the owner of the bucket it names follows it, and
/// answer what is served: with NO credential for a pre-signed address into this service's
/// own store, and with the caller's own credential scope for an address of their bucket.
async fn served(router: &MultiServiceRouter, case: &str, uri: &str) -> Vec<u8> {
    let (status, body) = if uri.contains("X-Amz-Signature") {
        fetch(router, uri).await
    } else {
        let (status, body) = s3(router, "GET", named_object(uri), &[]).await;
        (status, body.to_vec())
    };
    assert_eq!(
        status, 200,
        "case {case:?}: the reported location {uri:?} must serve its document, got HTTP \
         {status} with body {:?}",
        String::from_utf8_lossy(&body)
    );
    body
}

/// Every timestamp member a projection of this service carries, by name, so one assertion
/// covers each of them wherever it appears.
const INSTANTS: &[&str] = &["CreationTime", "StartTime", "CompletionTime"];

/// How many fractional digits a reported instant may carry: the milliseconds the members
/// document (see this module's header).
const MOST_FRACTIONAL_DIGITS: usize = 3;

/// Assert every instant `record` reports carries at most millisecond resolution.
fn assert_instant_resolution(case: &str, record: &Value) {
    for member in INSTANTS {
        let Some(instant) = record.get(member) else {
            continue;
        };
        if instant.is_null() {
            continue;
        }
        let number = instant
            .as_number()
            .unwrap_or_else(|| {
                panic!("case {case:?}: {member} is an epoch-seconds number, got {instant:?}")
            })
            .to_string();
        let digits = number
            .split_once('.')
            .map_or(0, |(_whole, fraction)| fraction.len());
        assert!(
            digits <= MOST_FRACTIONAL_DIGITS,
            "case {case:?}: {member} carries at most millisecond resolution, got {number} \
             with {digits} fractional digits"
        );
    }
}

/// The whole documented workflow, for every row: the start is accepted over audio this
/// emulator holds, the job reads back until it has finished, the location it reports serves
/// its transcript, and the delete is accepted and frees the name.
#[tokio::test]
async fn the_documented_workflow_over_uploaded_media_runs_from_start_to_delete() {
    let router = emulator();
    let rows = rows();
    // The output bucket is the caller's to create, exactly as the media bucket is.
    given_object(&router, OUTPUT_BUCKET, "placeholder", b"created by the caller").await;

    for row in &rows {
        let case = row.case;
        given_uploaded_audio(&router, row).await;

        // The start: accepted, with the job as it was stored.
        let started = call(&router, "StartTranscriptionJob", start_request(row)).await;
        let job = started.ok(case)["TranscriptionJob"].clone();
        assert_eq!(
            job["TranscriptionJobName"],
            json!(row.name),
            "case {case:?}: the started job carries the name the request asked for, got {job:?}"
        );
        assert_eq!(
            job["TranscriptionJobStatus"],
            json!("IN_PROGRESS"),
            "case {case:?}: a job the service is working on is IN_PROGRESS, got {job:?}"
        );
        assert_eq!(
            job["LanguageCode"],
            json!("en-US"),
            "case {case:?}: the started job carries the language the request named, got {job:?}"
        );
        assert_eq!(
            job["Media"],
            json!({"MediaFileUri": media_uri(row)}),
            "case {case:?}: the started job carries the media the request named, got {job:?}"
        );
        if let Some(format) = row.media_format {
            assert_eq!(
                job["MediaFormat"],
                json!(format),
                "case {case:?}: the started job carries the declared format, got {job:?}"
            );
        }
        for member in ["CreationTime", "StartTime"] {
            assert!(
                job[member].is_number(),
                "case {case:?}: a job that is processing reports its {member}, got {job:?}"
            );
        }
        assert_instant_resolution(case, &job);

        // The poll: the reader reports that same job until the clock has finished with it.
        let transcribe = Transcribe {
            router: std::sync::Arc::clone(&router),
        };
        let finished = transcribe.transcription_job_settled(row.name, None).await;
        assert_eq!(
            finished["TranscriptionJobStatus"],
            json!("COMPLETED"),
            "case {case:?}: a job over audio the store holds completes, got {finished:?}"
        );
        assert_eq!(
            finished["CreationTime"], job["CreationTime"],
            "case {case:?}: the creation instant of a stored job does not move, got {finished:?}"
        );
        assert_instant_resolution(case, &finished);

        // The location it reports: on this emulator's own endpoint, naming the object this
        // emulator stored, and serving the transcript of this very job.
        let reported = finished["Transcript"]["TranscriptFileUri"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: a COMPLETED job reports where its transcript is, got \
                     {finished:?}"
                )
            })
            .to_string();
        assert!(
            reported.starts_with(&format!("{UNSERVED_URL_BASE}/")),
            "case {case:?}: a reported location is published on the endpoint this emulator \
             is reached on, got {reported:?}"
        );
        let bucket = if row.service_managed {
            // This service's own store for the Region a request carrying no credential
            // scope is served in, as `crate::state::service_managed_bucket` spells it.
            "aws-transcribe-us-east-1-prod".to_string()
        } else {
            OUTPUT_BUCKET.to_string()
        };
        assert_eq!(
            named_object(&reported),
            format!("/{bucket}/{}", (row.transcript_key)(row.name)),
            "case {case:?}: the address names the object the transcript was stored at, got \
             {reported:?}"
        );
        let document = served(&router, case, &reported).await;
        let document: Value = serde_json::from_slice(&document).unwrap_or_else(|error| {
            panic!(
                "case {case:?}: the transcript document must be JSON ({error}): {:?}",
                String::from_utf8_lossy(&document)
            )
        });
        assert_eq!(
            document["jobName"],
            json!(row.name),
            "case {case:?}: the document served names its own job, got {document:?}"
        );

        // The delete: accepted, with an empty body, and the name is free again.
        let deleted = call(
            &router,
            "DeleteTranscriptionJob",
            json!({"TranscriptionJobName": row.name}),
        )
        .await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: the delete of a job this account holds is accepted, got HTTP {} \
             with body {:?}",
            deleted.status, deleted.body
        );
        assert_eq!(
            deleted.body,
            json!({}),
            "case {case:?}: the delete answers with an empty body, got {:?}",
            deleted.body
        );

        let gone = call(
            &router,
            "GetTranscriptionJob",
            json!({"TranscriptionJobName": row.name}),
        )
        .await;
        assert_eq!(
            gone.status, 400,
            "case {case:?}: a deleted job is no longer reported, got HTTP {} with body {:?}",
            gone.status, gone.body
        );
        assert_eq!(
            gone.body["Message"],
            json!(NO_SUCH_JOB),
            "case {case:?}: the refusal states that the job is not there, got {:?}",
            gone.body
        );
    }
}

/// The first read that reports a FINISHED job's location has already stored the document
/// there, in whatever Region the start was signed for: a client that polls to `COMPLETED`
/// and fetches straight away is served its transcript, so the location is never announced
/// before the object behind it exists.
#[tokio::test]
async fn the_first_read_that_reports_a_location_has_already_stored_its_document() {
    let router = emulator();
    // The Regions the rows are signed for: the one a request carrying no credential scope
    // falls back to, and another, whose service-managed store is its own bucket.
    for region in ["us-east-1", "us-west-2"] {
        let name = format!("first-read-{region}");
        let key = format!("regional/{region}.mp3");
        given_object(&router, MEDIA_BUCKET, &key, AUDIO).await;
        let request = json!({
            "TranscriptionJobName": name,
            "LanguageCode": "en-US",
            "Media": {"MediaFileUri": format!("s3://{MEDIA_BUCKET}/{key}")},
        });
        crate::transcription_jobs::dispatch(&router, "StartTranscriptionJob", request, Some(region))
            .await
            .ok(region);

        let transcribe = Transcribe {
            router: std::sync::Arc::clone(&router),
        };
        let finished = transcribe
            .transcription_job_settled(&name, Some(region))
            .await;
        assert_eq!(
            finished["TranscriptionJobStatus"],
            json!("COMPLETED"),
            "case {region:?}: a job over audio the store holds completes, got {finished:?}"
        );
        let reported = finished["Transcript"]["TranscriptFileUri"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {region:?}: a COMPLETED job reports its transcript, got {finished:?}")
            })
            .to_string();
        let document = served(&router, region, &reported).await;
        let document: Value = serde_json::from_slice(&document).unwrap_or_else(|error| {
            panic!(
                "case {region:?}: the transcript document must be JSON ({error}): {:?}",
                String::from_utf8_lossy(&document)
            )
        });
        assert_eq!(
            document["jobName"],
            json!(name),
            "case {region:?}: the document served names its own job, got {document:?}"
        );
    }
}

/// The instants a LISTING reports carry the same resolution the job itself reports: the
/// summary projection stamps them from the same record.
#[tokio::test]
async fn a_listed_job_reports_its_instants_at_the_same_resolution() {
    let router = emulator();
    let rows = rows();
    given_object(&router, OUTPUT_BUCKET, "placeholder", b"created by the caller").await;
    for row in &rows {
        given_uploaded_audio(&router, row).await;
        call(&router, "StartTranscriptionJob", start_request(row))
            .await
            .ok(row.case);
    }

    let transcribe = Transcribe {
        router: std::sync::Arc::clone(&router),
    };
    for row in &rows {
        transcribe.transcription_job_settled(row.name, None).await;
    }

    let listed = call(&router, "ListTranscriptionJobs", json!({"MaxResults": 100})).await;
    let summaries = listed.ok("every started job is listed")["TranscriptionJobSummaries"]
        .as_array()
        .unwrap_or_else(|| panic!("the listing answers with a list of summaries"))
        .clone();
    assert_eq!(
        summaries.len(),
        rows.len(),
        "the listing reports every job that was started, got {summaries:?}"
    );
    for summary in &summaries {
        let case = summary["TranscriptionJobName"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert_instant_resolution(&case, summary);
    }
}
