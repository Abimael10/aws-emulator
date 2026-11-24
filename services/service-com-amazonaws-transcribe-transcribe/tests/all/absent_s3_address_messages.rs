// SPDX-License-Identifier: CC0-1.0

//! WHICH absence a refusal about an Amazon S3 location names, end to end over the wire
//! with the emulator's own Amazon S3 mounted beside Transcribe on one engine — the way
//! the assembled emulator runs them.
//!
//! THE RULE THESE ROWS ASSERT: a refusal grounded in the object store's own
//! absent-address answer states what the MEMBER that named the location is about.
//!
//! * A location naming a FILE — the media a job start is pointed at, and the file a
//!   custom vocabulary, medical vocabulary or vocabulary filter is built from — is
//!   refused with the object sentence ([`URI_NAMES_NO_OBJECT`]) however the address fails
//!   to resolve: a bucket the store holds none of and a key a bucket that IS there holds
//!   none of alike, because either way the address names no file.
//! * A location naming a BUCKET for the job's OUTPUT (`OutputBucketName`) is refused with
//!   the bucket sentence ([`S3_BUCKET_DOES_NOT_EXIST`]) when the store holds no such
//!   bucket: there is nowhere for the results to be written. It is judged before the
//!   media, so a request whose output bucket and whose input media are both unresolvable
//!   is told that the bucket is not there.
//!
//! Both sentences are entirely FIXED texts carrying no value of the request, both ride
//! this service's modelled `BadRequestException` on HTTP 400, and either way NOTHING is
//! stored.
//!
//! The rows are a TABLE spanning what the rule generalises over: which operation names
//! the location (all four job starts, and the three creates that build a resource from a
//! file), which member names it (`Media.MediaFileUri`, the `Media.RedactedMediaFileUri`
//! Call Analytics accepts instead, `VocabularyFileUri`, `VocabularyFilterFileUri`,
//! `OutputBucketName`), and how the address fails to resolve (an absent bucket with a
//! root key and with a nested key under it, an absent key at the bucket root and under a
//! nested prefix, and a key that only prefixes a stored one).
//!
//! The last test is the control that keeps the refusals honest: the same requests over
//! the staged object are accepted and read back, so what the rows above refuse is the
//! ABSENCE and not the shape of the request.
//!
//! The harness is the shared one (`crate::transcription_jobs::Transcribe`) in its
//! with-the-object-store form, reused rather than stood up a second time.

use serde_json::{json, Value};

use crate::media_object_resolution::ABSENT_BUCKET;
use crate::transcription_jobs::Transcribe;

/// The caller's own bucket: created and uploaded to before any request names it.
const BUCKET: &str = "example-transcribe-inputs";

/// The objects staged in [`BUCKET`]: one at the bucket root, one under a nested prefix.
const STAGED_KEYS: &[&str] = &["dictation.wav", "clinic/visits/terms.txt"];

/// What each staged object holds: one entry per line, which is the form every member
/// that names a terms file documents. A media object's bytes are never read — nothing
/// here transcribes audio — so the same content serves both kinds of row.
const STAGED_BYTES: &[u8] = b"Aardvark\nTranscribe\n";

/// The refusal a start that directs its output into a bucket the store holds none of
/// carries, as the service words that error class.
///
/// `pub(crate)`: the suites over the media-resolution and start-precondition rules
/// (`crate::media_object_resolution`, `crate::transcription_job_absent_media_state`,
/// `crate::call_analytics_job_start_preconditions`) read this service's absent-address
/// sentences from here rather than keeping copies of them.
pub(crate) const S3_BUCKET_DOES_NOT_EXIST: &str =
    "The specified S3 bucket doesn't exist. Make sure the bucket exists and try your \
     request again.";

/// The refusal an address naming a FILE the store holds none of carries — every absent
/// address of a terms file, and every absent address of a job's media.
pub(crate) const URI_NAMES_NO_OBJECT: &str =
    "The URI that you provided doesn't point to an S3 object. Make sure that the object \
     exists and try your request again.";

/// The one `LanguageCode` every family here accepts, and the only one Amazon Transcribe
/// Medical supports ("US English (en-US) is the only valid value for medical
/// transcription jobs").
const LANGUAGE_CODE: &str = "en-US";

/// The role a Medical Scribe start names, in the form `DataAccessRoleArn`'s `@pattern`
/// pins.
const ROLE_ARN: &str = "arn:aws:iam::111122223333:role/TranscribeAccess";

/// The status this service puts on every client error.
const CLIENT_ERROR_STATUS: u16 = 400;

/// The shape every refusal here carries.
const CLIENT_ERROR_SHAPE: &str = "BadRequestException";

/// One operation that names an Amazon S3 location of a FILE, with the reader that proves
/// whether anything was stored.
struct Row {
    /// What this row varies, named in every assertion.
    case: &'static str,
    operation: &'static str,
    /// This operation's request, naming the resource `name` and `uri` as its S3
    /// location.
    request: fn(&str, &str) -> Value,
    read: &'static str,
    /// The member the request and the reader name the resource with.
    name_member: &'static str,
}

/// Every operation of this service whose request is validated against an Amazon S3
/// address.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a transcription job's media",
            operation: "StartTranscriptionJob",
            request: |name, uri| {
                json!({
                    "TranscriptionJobName": name,
                    "LanguageCode": LANGUAGE_CODE,
                    "Media": {"MediaFileUri": uri}
                })
            },
            read: "GetTranscriptionJob",
            name_member: "TranscriptionJobName",
        },
        Row {
            case: "a medical transcription job's media",
            operation: "StartMedicalTranscriptionJob",
            request: |name, uri| {
                json!({
                    "MedicalTranscriptionJobName": name,
                    "LanguageCode": LANGUAGE_CODE,
                    "Media": {"MediaFileUri": uri},
                    "OutputBucketName": BUCKET,
                    "Specialty": "PRIMARYCARE",
                    "Type": "CONVERSATION"
                })
            },
            read: "GetMedicalTranscriptionJob",
            name_member: "MedicalTranscriptionJobName",
        },
        Row {
            case: "a Call Analytics job naming its input audio in RedactedMediaFileUri",
            operation: "StartCallAnalyticsJob",
            request: |name, uri| {
                json!({
                    "CallAnalyticsJobName": name,
                    "Media": {"RedactedMediaFileUri": uri}
                })
            },
            read: "GetCallAnalyticsJob",
            name_member: "CallAnalyticsJobName",
        },
        Row {
            case: "a Medical Scribe job's media",
            operation: "StartMedicalScribeJob",
            request: |name, uri| {
                json!({
                    "MedicalScribeJobName": name,
                    "Media": {"MediaFileUri": uri},
                    "OutputBucketName": BUCKET,
                    "DataAccessRoleArn": ROLE_ARN,
                    "Settings": {"ShowSpeakerLabels": true, "MaxSpeakerLabels": 2}
                })
            },
            read: "GetMedicalScribeJob",
            name_member: "MedicalScribeJobName",
        },
        Row {
            case: "a custom vocabulary's table file",
            operation: "CreateVocabulary",
            request: |name, uri| {
                json!({
                    "VocabularyName": name,
                    "LanguageCode": LANGUAGE_CODE,
                    "VocabularyFileUri": uri
                })
            },
            name_member: "VocabularyName",
            read: "GetVocabulary",
        },
        Row {
            case: "a custom medical vocabulary's table file",
            operation: "CreateMedicalVocabulary",
            request: |name, uri| {
                json!({
                    "VocabularyName": name,
                    "LanguageCode": LANGUAGE_CODE,
                    "VocabularyFileUri": uri
                })
            },
            name_member: "VocabularyName",
            read: "GetMedicalVocabulary",
        },
        Row {
            case: "a custom vocabulary filter's word file",
            operation: "CreateVocabularyFilter",
            request: |name, uri| {
                json!({
                    "VocabularyFilterName": name,
                    "LanguageCode": LANGUAGE_CODE,
                    "VocabularyFilterFileUri": uri
                })
            },
            name_member: "VocabularyFilterName",
            read: "GetVocabularyFilter",
        },
    ]
}

/// One way an address the object store answers about resolves to nothing.
struct Absence {
    /// What this row varies, named in every assertion.
    case: &'static str,
    uri: fn() -> String,
}

/// Every way the store itself answers that nothing is at the address, crossed with the
/// shape of the key: the bucket is absent (so no key under it can be there), or the
/// bucket is there and the key is not.
fn absences() -> Vec<Absence> {
    vec![
        Absence {
            case: "a bucket the store does not hold, with a key at its root",
            uri: || format!("s3://{ABSENT_BUCKET}/{}", STAGED_KEYS[0]),
        },
        Absence {
            case: "a bucket the store does not hold, with a nested key under it",
            uri: || format!("s3://{ABSENT_BUCKET}/{}", STAGED_KEYS[1]),
        },
        Absence {
            case: "a key the staged bucket does not hold, at its root",
            uri: || format!("s3://{BUCKET}/never-uploaded.wav"),
        },
        Absence {
            case: "a key the staged bucket does not hold, under a nested prefix",
            uri: || format!("s3://{BUCKET}/clinic/visits/never-uploaded.txt"),
        },
        Absence {
            case: "a key that only prefixes a stored one",
            uri: || format!("s3://{BUCKET}/dictation"),
        },
    ]
}

/// One emulator with the object store, holding [`STAGED_KEYS`] in [`BUCKET`] — the
/// upload a caller performs before naming a location in a request.
async fn emulator_with_staged_objects() -> Transcribe {
    let transcribe = Transcribe::with_object_store();
    for key in STAGED_KEYS {
        transcribe.given_object(BUCKET, key, STAGED_BYTES).await;
    }
    transcribe
}

/// RULE: an address naming a FILE that resolves to nothing is refused with the object
/// sentence — whichever operation named it, and whichever part of the address the store
/// holds none of — and nothing is stored.
#[tokio::test]
async fn a_refusal_about_an_absent_s3_file_names_the_object() {
    let transcribe = emulator_with_staged_objects().await;

    for row in rows() {
        for (index, absence) in absences().iter().enumerate() {
            let case = format!("{} / {}", row.case, absence.case);
            let name = format!("{}-{index}", row.operation);
            let uri = (absence.uri)();

            let refused = transcribe
                .call(row.operation, (row.request)(&name, &uri))
                .await;
            refused.refused(&case, CLIENT_ERROR_STATUS, CLIENT_ERROR_SHAPE);
            let message = refused.error_message(&case);
            assert_eq!(
                message, URI_NAMES_NO_OBJECT,
                "case {case:?}: expected the refusal {URI_NAMES_NO_OBJECT:?}, got \
                 {message:?}"
            );
            // The texts are the service's own: no part of the address the caller named
            // appears in them.
            assert!(
                !message.contains(&uri) && !message.contains(ABSENT_BUCKET),
                "case {case:?}: the refusal must carry no value of the request, got \
                 {message:?}"
            );

            // Refused before anything was recorded: the family's own reader finds
            // nothing under the name.
            transcribe
                .call(row.read, json!({row.name_member: &name}))
                .await
                .refused(&case, CLIENT_ERROR_STATUS, CLIENT_ERROR_SHAPE);
        }
    }
}

/// One operation that names a BUCKET of the caller's for the output of the job it starts
/// (`OutputBucketName`), with the reader that proves whether anything was stored.
struct OutputRow {
    /// What this row varies, named in every assertion.
    case: &'static str,
    operation: &'static str,
    /// This operation's request, naming the job `name`, its media `uri` and `bucket` as
    /// the bucket its output is to be written into.
    request: fn(&str, &str, &str) -> Value,
    read: &'static str,
    /// The member the request and the reader name the job with.
    name_member: &'static str,
}

/// Every operation of this service that takes an `OutputBucketName`: the one where the
/// member is optional and the two where it is `@required`
/// (`smithy_query(query="operation", operation="StartTranscriptionJob,\
/// StartMedicalTranscriptionJob,StartMedicalScribeJob")`). `StartCallAnalyticsJob` takes
/// none, naming a whole `OutputLocation` URI instead.
fn output_rows() -> Vec<OutputRow> {
    vec![
        OutputRow {
            case: "a transcription job's output bucket",
            operation: "StartTranscriptionJob",
            request: |name, uri, bucket| {
                json!({
                    "TranscriptionJobName": name,
                    "LanguageCode": LANGUAGE_CODE,
                    "Media": {"MediaFileUri": uri},
                    "OutputBucketName": bucket
                })
            },
            read: "GetTranscriptionJob",
            name_member: "TranscriptionJobName",
        },
        OutputRow {
            case: "a medical transcription job's required output bucket",
            operation: "StartMedicalTranscriptionJob",
            request: |name, uri, bucket| {
                json!({
                    "MedicalTranscriptionJobName": name,
                    "LanguageCode": LANGUAGE_CODE,
                    "Media": {"MediaFileUri": uri},
                    "OutputBucketName": bucket,
                    "Specialty": "PRIMARYCARE",
                    "Type": "CONVERSATION"
                })
            },
            read: "GetMedicalTranscriptionJob",
            name_member: "MedicalTranscriptionJobName",
        },
        OutputRow {
            case: "a Medical Scribe job's required output bucket",
            operation: "StartMedicalScribeJob",
            request: |name, uri, bucket| {
                json!({
                    "MedicalScribeJobName": name,
                    "Media": {"MediaFileUri": uri},
                    "OutputBucketName": bucket,
                    "DataAccessRoleArn": ROLE_ARN,
                    "Settings": {"ShowSpeakerLabels": true, "MaxSpeakerLabels": 2}
                })
            },
            read: "GetMedicalScribeJob",
            name_member: "MedicalScribeJobName",
        },
    ]
}

/// RULE: a start directed at an output bucket the store holds none of is refused with the
/// BUCKET sentence, whatever its media says — the member names a bucket, so that is the
/// absence the refusal states — and nothing is stored.
///
/// The media varies along the rows: one staged object, and one address that resolves to
/// nothing (whose own refusal would be the object sentence), which is what pins the order
/// the two are judged in.
#[tokio::test]
async fn a_start_directed_at_an_absent_output_bucket_names_the_bucket() {
    let transcribe = emulator_with_staged_objects().await;
    let media = [
        ("media the store holds", format!("s3://{BUCKET}/{}", STAGED_KEYS[0])),
        (
            "media the store does not hold either",
            format!("s3://{ABSENT_BUCKET}/{}", STAGED_KEYS[0]),
        ),
    ];

    for row in output_rows() {
        for (index, (media_case, uri)) in media.iter().enumerate() {
            let case = format!("{} / {media_case}", row.case);
            let name = format!("{}-absent-output-{index}", row.operation);

            let refused = transcribe
                .call(row.operation, (row.request)(&name, uri, ABSENT_BUCKET))
                .await;
            refused.refused(&case, CLIENT_ERROR_STATUS, CLIENT_ERROR_SHAPE);
            let message = refused.error_message(&case);
            assert_eq!(
                message, S3_BUCKET_DOES_NOT_EXIST,
                "case {case:?}: expected the refusal {S3_BUCKET_DOES_NOT_EXIST:?}, got \
                 {message:?}"
            );
            assert!(
                !message.contains(ABSENT_BUCKET),
                "case {case:?}: the refusal must carry no value of the request, got \
                 {message:?}"
            );

            // Refused before anything was recorded.
            transcribe
                .call(row.read, json!({row.name_member: &name}))
                .await
                .refused(&case, CLIENT_ERROR_STATUS, CLIENT_ERROR_SHAPE);
        }
    }
}

/// The other side of the same rule: with the output bucket one the store HOLDS, the same
/// starts are judged by their media — an unresolvable media address is refused with the
/// object sentence, and a staged one is accepted and reads back.
#[tokio::test]
async fn a_start_whose_output_bucket_is_held_is_judged_by_its_media() {
    let transcribe = emulator_with_staged_objects().await;

    for row in output_rows() {
        let case = format!("{} / held, over unresolvable media", row.case);
        let name = format!("{}-held-output-absent-media", row.operation);
        let refused = transcribe
            .call(
                row.operation,
                (row.request)(
                    &name,
                    &format!("s3://{BUCKET}/never-uploaded.wav"),
                    BUCKET,
                ),
            )
            .await;
        refused.refused(&case, CLIENT_ERROR_STATUS, CLIENT_ERROR_SHAPE);
        let message = refused.error_message(&case);
        assert_eq!(
            message, URI_NAMES_NO_OBJECT,
            "case {case:?}: expected the refusal {URI_NAMES_NO_OBJECT:?}, got {message:?}"
        );

        let case = format!("{} / held, over staged media", row.case);
        let name = format!("{}-held-output-staged-media", row.operation);
        transcribe
            .call(
                row.operation,
                (row.request)(&name, &format!("s3://{BUCKET}/{}", STAGED_KEYS[0]), BUCKET),
            )
            .await
            .ok_status(&case, 200);
        transcribe
            .call(row.read, json!({row.name_member: &name}))
            .await
            .ok(&case);
    }
}

/// The control: the very same requests over an address the store ANSWERS for are
/// accepted and read back, so the rows above are refused for the absence and not for
/// the shape of the request.
#[tokio::test]
async fn an_s3_address_the_store_answers_for_is_accepted_and_reads_back() {
    let transcribe = emulator_with_staged_objects().await;

    for row in rows() {
        for (index, key) in STAGED_KEYS.iter().enumerate() {
            let case = format!("{} / the staged object {key}", row.case);
            let name = format!("{}-staged-{index}", row.operation);
            let uri = format!("s3://{BUCKET}/{key}");

            transcribe
                .call(row.operation, (row.request)(&name, &uri))
                .await
                .ok_status(&case, 200);

            // Accepted means STORED: the family's own reader answers for the name.
            transcribe
                .call(row.read, json!({row.name_member: &name}))
                .await
                .ok(&case);
        }
    }
}
