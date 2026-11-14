// SPDX-License-Identifier: CC0-1.0

//! WHICH absence a refusal about an Amazon S3 location names, end to end over the wire
//! with the emulator's own Amazon S3 mounted beside Transcribe on one engine — the way
//! the assembled emulator runs them.
//!
//! THE RULE THESE ROWS ASSERT: a refusal grounded in the object store's own
//! absent-address answer states what the store said is absent, as the member the request
//! named the location in words it.
//!
//! * The MEDIA a job start is pointed at distinguishes the two: a bucket the store holds
//!   none of is refused with the bucket sentence ([`S3_BUCKET_DOES_NOT_EXIST`]), because
//!   the caller has nowhere to have uploaded any media yet, while an address whose bucket
//!   is there and whose object is not is refused with the object sentence
//!   ([`URI_NAMES_NO_OBJECT`]).
//! * The FILE a custom vocabulary, medical vocabulary or vocabulary filter is built from
//!   makes no such distinction: those members are about the file, and their recorded
//!   refusal is the object sentence however the address fails to resolve — the bucket
//!   included.
//!
//! Both sentences are entirely FIXED texts carrying no value of the request, both ride
//! this service's modelled `BadRequestException` on HTTP 400, and either way NOTHING is
//! stored.
//!
//! The rows are a TABLE spanning what the rule generalises over: which operation names
//! the location (all four job starts, and the three creates that build a resource from a
//! file), which member names it (`Media.MediaFileUri`, the `Media.RedactedMediaFileUri`
//! Call Analytics accepts instead, `VocabularyFileUri`, `VocabularyFilterFileUri`), and
//! how the address fails to resolve (an absent bucket with a root key and with a nested
//! key under it, an absent key at the bucket root and under a nested prefix, and a key
//! that only prefixes a stored one).
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

/// The refusal a job start whose media is in a bucket the store holds none of carries,
/// as the service words that error class.
///
/// `pub(crate)`: the suites over the media-resolution and start-precondition rules
/// (`crate::media_object_resolution`, `crate::transcription_job_absent_media_state`,
/// `crate::call_analytics_job_start_preconditions`) assert the same sentence and read it
/// from here rather than keeping copies of it.
pub(crate) const S3_BUCKET_DOES_NOT_EXIST: &str =
    "The specified S3 bucket doesn't exist. Make sure the bucket exists and try your \
     request again.";

/// The refusal an address whose object the store holds none of carries — every absent
/// address of a terms file, and a job's media in a bucket that IS there.
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

/// One operation that names an Amazon S3 location, with the reader that proves whether
/// anything was stored.
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
    /// The sentence this operation's own member words an absent BUCKET with: the bucket
    /// one for the media a job start is pointed at, the object one for the file a
    /// resource is built from.
    absent_bucket: &'static str,
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
            absent_bucket: S3_BUCKET_DOES_NOT_EXIST,
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
            absent_bucket: S3_BUCKET_DOES_NOT_EXIST,
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
            absent_bucket: S3_BUCKET_DOES_NOT_EXIST,
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
            absent_bucket: S3_BUCKET_DOES_NOT_EXIST,
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
            read: "GetVocabulary",
            name_member: "VocabularyName",
            absent_bucket: URI_NAMES_NO_OBJECT,
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
            read: "GetMedicalVocabulary",
            name_member: "VocabularyName",
            absent_bucket: URI_NAMES_NO_OBJECT,
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
            read: "GetVocabularyFilter",
            name_member: "VocabularyFilterName",
            absent_bucket: URI_NAMES_NO_OBJECT,
        },
    ]
}

/// One way an address the object store answers about resolves to nothing.
struct Absence {
    /// What this row varies, named in every assertion.
    case: &'static str,
    uri: fn() -> String,
    /// Whether the part the store holds none of is the BUCKET, which is what the two
    /// sentences differ over.
    bucket_is_absent: bool,
}

/// Every way the store itself answers that nothing is at the address, crossed with the
/// shape of the key: the bucket is absent (so no key under it can be there), or the
/// bucket is there and the key is not.
fn absences() -> Vec<Absence> {
    vec![
        Absence {
            case: "a bucket the store does not hold, with a key at its root",
            uri: || format!("s3://{ABSENT_BUCKET}/{}", STAGED_KEYS[0]),
            bucket_is_absent: true,
        },
        Absence {
            case: "a bucket the store does not hold, with a nested key under it",
            uri: || format!("s3://{ABSENT_BUCKET}/{}", STAGED_KEYS[1]),
            bucket_is_absent: true,
        },
        Absence {
            case: "a key the staged bucket does not hold, at its root",
            uri: || format!("s3://{BUCKET}/never-uploaded.wav"),
            bucket_is_absent: false,
        },
        Absence {
            case: "a key the staged bucket does not hold, under a nested prefix",
            uri: || format!("s3://{BUCKET}/clinic/visits/never-uploaded.txt"),
            bucket_is_absent: false,
        },
        Absence {
            case: "a key that only prefixes a stored one",
            uri: || format!("s3://{BUCKET}/dictation"),
            bucket_is_absent: false,
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

/// RULE: the refusal states what the object store said is absent, as the member that
/// named the location words it — for every operation that resolves an S3 address — and
/// nothing is stored either way.
#[tokio::test]
async fn a_refusal_about_an_absent_s3_address_names_what_is_absent() {
    let transcribe = emulator_with_staged_objects().await;

    for row in rows() {
        for (index, absence) in absences().iter().enumerate() {
            let case = format!("{} / {}", row.case, absence.case);
            let name = format!("{}-{index}", row.operation);
            let uri = (absence.uri)();
            let expected = if absence.bucket_is_absent {
                row.absent_bucket
            } else {
                URI_NAMES_NO_OBJECT
            };

            let refused = transcribe
                .call(row.operation, (row.request)(&name, &uri))
                .await;
            refused.refused(&case, CLIENT_ERROR_STATUS, CLIENT_ERROR_SHAPE);
            let message = refused.error_message(&case);
            assert_eq!(
                message, expected,
                "case {case:?}: expected the refusal {expected:?}, got {message:?}"
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
