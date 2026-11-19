// SPDX-License-Identifier: CC0-1.0

//! WHAT a location this service hands a caller to FETCH actually is, end to end over the
//! wire: every `DownloadUri` and every job output location, followed by a fetch of that
//! very address made the way a client makes it — with no credential of the client's own.
//!
//! THE RULE THESE ROWS ASSERT: a location this service reports for a file in its OWN
//! service-managed store is a temporary, pre-signed address on the endpoint this emulator
//! is reached on, so a client that follows it with no credential of its own is served the
//! stored file. The model states the contract for one of them — "If you didn't include
//! `OutputBucketName` in your transcription job request, your transcript is stored in a
//! service-managed bucket, and `TranscriptFileUri` provides you with a temporary URI you
//! can use for secure access to your transcript", with "Temporary URIs for service-managed
//! Amazon S3 buckets … only valid for 15 minutes"
//! (`smithy_query(query="doc", shape="Transcript", member="TranscriptFileUri")`) — and a
//! `DownloadUri` is the same kind of location ("use this URI to view or download the
//! custom vocabulary", `smithy_query(query="operation", operation="GetVocabulary")`). A
//! location in a bucket the REQUEST named is the plain, UNSIGNED address of that object on
//! the same endpoint instead: they created that bucket and address it with their own
//! credentials, so nothing is signed for it. WHICH of the two a record holds follows from
//! the stored request — whether the start named a bucket or an output location of its own
//! — and not from the text of the address, which is why the caller-bucket table below holds
//! rows whose bucket and key IMITATE this service's own store and are still reported as
//! the caller's.
//!
//! WHY the signature is the point: an unsigned address into a private bucket is a request
//! by nobody, which the object store refuses — so a reported location carrying none can
//! never be read, however correct its bucket and key. The last test pins that directly,
//! with request authentication ON (`set_request_authentication(true)`, the `--auth on`
//! path the emulator binary runs): the address as reported is served, and the same address
//! stripped of its signature, or carrying another, is not.
//!
//! The rows differ along what the rule generalises over: WHICH reported member carries the
//! location (a custom vocabulary's `DownloadUri`, a vocabulary filter's, a custom medical
//! vocabulary's, a job's `Transcript.TranscriptFileUri`, its
//! `Transcript.RedactedTranscriptFileUri`, each entry of `SubtitlesOutput.SubtitleFileUris`
//! and a Call Analytics job's transcript), whose bucket the file is in (this service's own
//! against one the request named), and which document kind sits at the address (a text file
//! of terms, a transcript document, a WebVTT file, a SubRip file).
//!
//! The emulator mounted here holds the Amazon S3 service beside transcribe on ONE shared
//! engine, which is what the assembled router does, since that store is where these files
//! are written and fetched from (`crate::media_object_resolution::emulator`, reused rather
//! than stood up again).

use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};
use smithy_server_runtime::{MultiServiceRouter, UNSERVED_URL_BASE};

use crate::media_object_resolution::{call, emulator, given_object, given_staged_media, BUCKET};

/// The Region every call of this suite is signed for, which is the Region
/// `crate::media_object_resolution`'s harness serves and so the Region whose
/// service-managed bucket these files live in.
const REGION: &str = "us-east-1";

/// The bucket this service's OWN files live in for [`REGION`], as
/// `crate::state::service_managed_bucket` spells it — written out here so a change to that
/// name has to be made deliberately in both places.
///
/// `pub(crate)`: the suite over WHICH endpoint a location is published on
/// (`crate::reported_location_endpoints`) names the same bucket, through this one spelling.
pub(crate) fn service_managed_bucket() -> String {
    format!("aws-transcribe-{REGION}-prod")
}

/// A bucket of the CALLER's, which the caller creates before naming it in a start request.
const CALLER_BUCKET: &str = "example-reported-output";

/// The media every job row here is pointed at: one of the objects the shared harness
/// stages ([`given_staged_media`]).
fn media() -> Value {
    json!({"MediaFileUri": format!("s3://{BUCKET}/dictation.wav")})
}

/// The bucket and key of the vocabulary TABLE a custom medical vocabulary is built from —
/// the caller's own object, uploaded before calling ("you must first upload a text file
/// that contains your vocabulary table into an Amazon S3 bucket").
const TABLE_BUCKET: &str = "example-reported-tables";
/// See [`TABLE_BUCKET`].
const TABLE_KEY: &str = "cardiology.txt";

/// What that table holds: the `Phrase IPA SoundsLike DisplayAs` column header
/// `CreateMedicalVocabulary` documents, and one row under it.
const TABLE: &[u8] = b"Phrase\tIPA\tSoundsLike\tDisplayAs\n\
                       Tympanic-membrane\t\t\tTympanic membrane\n";

/// How long a poll loop keeps looking before it gives up, and how long it waits between
/// two looks. Generous: what is waited for is a transition this service owes in half a
/// second, and nothing here is timed — the loop stops as soon as the condition holds.
const POLL_DEADLINE: Duration = Duration::from_secs(30);
/// See [`POLL_DEADLINE`].
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The six query fields a SigV4 query-string signature puts on an address, in the order
/// AWS documents them (AWS General Reference, *Authenticating Requests: Using Query
/// Parameters*).
const SIGNING_FIELDS: [&str; 6] = [
    "X-Amz-Algorithm",
    "X-Amz-Credential",
    "X-Amz-Date",
    "X-Amz-Expires",
    "X-Amz-SignedHeaders",
    "X-Amz-Signature",
];

/// The algorithm such a signature names.
const SIGV4_ALGORITHM: &str = "AWS4-HMAC-SHA256";

/// The window the model documents for a temporary URI into the service-managed store, in
/// seconds: "Temporary URIs for service-managed Amazon S3 buckets are only valid for 15
/// minutes" (model doc for `Transcript$TranscriptFileUri`).
const SERVICE_MANAGED_VALIDITY_SECONDS: u64 = 15 * 60;

/// The OBJECT a reported location names, as the object store addresses it: the
/// `/BUCKET/KEY` path the address carries, for both kinds of location this service reports
/// — a temporary address into its own store and the unsigned address of an object in a
/// bucket the request named.
///
/// `pub(crate)`: every suite that pins WHERE this service reports a file reads the object
/// out of the reported location through this one helper, because a service-managed
/// address is TEMPORARY — the signature it carries and the instant it was signed at are
/// not part of which object it names.
pub(crate) fn named_object(uri: &str) -> &str {
    let address = uri.split('?').next().unwrap_or(uri);
    match address
        .strip_prefix("https://")
        .or_else(|| address.strip_prefix("http://"))
    {
        Some(rest) => rest.find('/').map_or(address, |at| &rest[at..]),
        None => address,
    }
}

/// `record` with every TEMPORARY location it reports reduced to the OBJECT that address
/// names ([`named_object`]), at whatever depth the projection carries it; everything else
/// is left exactly as reported.
///
/// `pub(crate)`: what two reads of ONE settled record must agree on is what the record
/// reports, and a location in this service's own store is a temporary pre-signed address —
/// each read mints a fresh signature over the same object, at the instant of that read (see
/// this module's header). So two reads are compared through here: compared as reported, two
/// reads of an unchanged record agree only while both fall in the same second, which makes
/// the comparison hold or fail by nothing but how fast the host ran. The WHOLE query of such
/// an address goes, not only its signature: the signing instant is in `X-Amz-Date` and the
/// credential scope carries that day as well. What each of those fields must be is this
/// suite's own subject instead, asserted on the address as reported.
pub(crate) fn reported_objects(record: &Value) -> Value {
    match record {
        Value::String(text) if is_temporary_address(text) => json!(named_object(text)),
        Value::Array(values) => Value::Array(values.iter().map(reported_objects).collect()),
        Value::Object(members) => Value::Object(
            members
                .iter()
                .map(|(name, value)| (name.clone(), reported_objects(value)))
                .collect(),
        ),
        reported => reported.clone(),
    }
}

/// Whether `text` is an address carrying a SigV4 query signature, which is what a temporary
/// location into this service's own store is: it names every field of [`SIGNING_FIELDS`].
/// An address of the caller's own bucket carries none of them and is left as reported.
fn is_temporary_address(text: &str) -> bool {
    SIGNING_FIELDS
        .iter()
        .all(|name| !field(text, name).is_empty())
}

/// The query of a reported address, or the empty string when it carries none.
fn query_of(uri: &str) -> &str {
    uri.split_once('?').map_or("", |(_path, query)| query)
}

/// The value of query field `name` in `uri`, exactly as it was written.
fn field(uri: &str, name: &str) -> String {
    query_of(uri)
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find_map(|(field, value)| (field == name).then(|| value.to_string()))
        .unwrap_or_default()
}

/// Fetch `uri` the way a client follows a location it was handed: the address exactly as
/// reported, the `Host` it names, and NO credential of the client's own.
///
/// `pub(crate)`, like [`named_object`]: the suite over WHICH endpoint a location is
/// published on (`crate::reported_location_endpoints`) follows the addresses it reads
/// through this same one request, so the two cannot disagree about what following a
/// reported location means.
pub(crate) async fn fetch(router: &MultiServiceRouter, uri: &str) -> (u16, Vec<u8>) {
    let host = uri
        .split_once("://")
        .and_then(|(_scheme, rest)| rest.split('/').next())
        .unwrap_or_else(|| panic!("a reported location names a host, got {uri:?}"))
        .to_string();
    let request = http::Request::builder()
        .method("GET")
        .uri(uri)
        .header("host", host)
        .body(bytes::Bytes::new())
        .expect("the request builds");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        response.body().as_ref().to_vec(),
    )
}

/// Poll `operation` until the value at `member` of the answer's `holder` object is one of
/// `settled`, and answer that whole holder object.
async fn settled(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    input: Value,
    holder: Option<&str>,
    member: &str,
    settled: &[&str],
) -> Value {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let reply = call(router, operation, input.clone()).await;
        let body = reply.ok(case).clone();
        let held = match holder {
            Some(holder) => body[holder].clone(),
            None => body,
        };
        let state = held[member].as_str().unwrap_or_default().to_string();
        if settled.contains(&state.as_str()) {
            return held;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {operation} never reported {member} in {settled:?}, last saw \
             {state:?}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// What the file at a reported location must hold.
#[derive(Debug, Clone)]
enum Expected {
    /// A text file of terms, one per line, in the order the build listed them.
    Lines(Vec<String>),
    /// The transcript document of the job named here, which carries the job, the account
    /// and the status the job finished in.
    Transcript(String),
    /// A file whose exact text is known (a subtitle file).
    Text(String),
}

/// One reported location: what reports it, the address as reported, the object the address
/// must name and what is served there.
struct Reported {
    case: String,
    uri: String,
    /// The object the address names, relative to the store: `/<bucket>/<key>`.
    object: String,
    expected: Expected,
}

/// Assert what is served at a reported location, over a credential-free fetch of the
/// address exactly as reported.
async fn assert_serves(router: &MultiServiceRouter, row: &Reported) {
    let case = &row.case;
    let (status, body) = fetch(router, &row.uri).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the reported location {:?} must serve its file to a client \
         carrying no credential of its own, got HTTP {status} with body {:?}",
        row.uri,
        String::from_utf8_lossy(&body)
    );
    let text = String::from_utf8_lossy(&body).to_string();
    match &row.expected {
        Expected::Lines(expected) => {
            let served: Vec<String> = text.lines().map(str::to_string).collect();
            assert_eq!(
                &served, expected,
                "case {case:?}: expected the file at {:?} to hold {expected:?}, got \
                 {served:?}",
                row.uri
            );
        }
        Expected::Transcript(job) => {
            let document: Value = serde_json::from_str(&text).unwrap_or_else(|error| {
                panic!("case {case:?}: the transcript document must be JSON ({error}): {text:?}")
            });
            assert_eq!(
                document["jobName"],
                json!(job),
                "case {case:?}: the transcript document names its job, got {document:?}"
            );
            assert_eq!(
                document["status"],
                json!("COMPLETED"),
                "case {case:?}: the transcript document states the status the job finished \
                 in, got {document:?}"
            );
            assert!(
                document["results"]["transcripts"][0]["transcript"].is_string()
                    && document["results"]["items"].is_array(),
                "case {case:?}: the transcript document carries a results envelope, got \
                 {document:?}"
            );
        }
        Expected::Text(expected) => assert_eq!(
            &text, expected,
            "case {case:?}: expected the file at {:?} to hold {expected:?}, got {text:?}",
            row.uri
        ),
    }
}

/// Build one of everything that reports a fetchable location in this service's OWN store,
/// and answer every location each one reports.
async fn service_managed_locations(router: &MultiServiceRouter) -> Vec<Reported> {
    let account = engine::emulator_account_id();
    let bucket = service_managed_bucket();
    let object = |key: String| format!("/{bucket}/{key}");

    // The three families that report a `DownloadUri`. The first two take their content
    // inline; a custom MEDICAL vocabulary's terms can only come from a table file, so its
    // own is staged first.
    let phrases = vec!["Alderaan".to_string(), "Tatooine".to_string()];
    let words = vec!["dang".to_string(), "heck".to_string()];
    call(
        router,
        "CreateVocabulary",
        json!({
            "VocabularyName": "served-vocabulary",
            "LanguageCode": "en-US",
            "Phrases": phrases,
        }),
    )
    .await
    .ok("a custom vocabulary built from its request's own phrases");
    call(
        router,
        "CreateVocabularyFilter",
        json!({
            "VocabularyFilterName": "served-filter",
            "LanguageCode": "en-US",
            "Words": words,
        }),
    )
    .await
    .ok("a vocabulary filter built from its request's own words");
    given_object(router, TABLE_BUCKET, TABLE_KEY, TABLE).await;
    call(
        router,
        "CreateMedicalVocabulary",
        json!({
            "VocabularyName": "served-medical-vocabulary",
            "LanguageCode": "en-US",
            "VocabularyFileUri": format!("s3://{TABLE_BUCKET}/{TABLE_KEY}"),
        }),
    )
    .await
    .ok("a custom medical vocabulary built from its table file");

    // A transcription job that named no output bucket, asking for both transcripts and
    // both subtitle formats, so one job reports every member of its family that carries a
    // location; and a Call Analytics job that named no output location.
    call(
        router,
        "StartTranscriptionJob",
        json!({
            "TranscriptionJobName": "served-job",
            "LanguageCode": "en-US",
            "Media": media(),
            "Subtitles": {"Formats": ["vtt", "srt"]},
            "ContentRedaction": {
                "RedactionType": "PII",
                "RedactionOutput": "redacted_and_unredacted",
            },
        }),
    )
    .await
    .ok("a transcription job whose output is this service's own");
    call(
        router,
        "StartCallAnalyticsJob",
        json!({"CallAnalyticsJobName": "served-call", "Media": media()}),
    )
    .await
    .ok("a Call Analytics job whose output is this service's own");

    let mut rows = Vec::new();

    let vocabulary = settled(
        router,
        "a custom vocabulary's DownloadUri",
        "GetVocabulary",
        json!({"VocabularyName": "served-vocabulary"}),
        None,
        "VocabularyState",
        &["READY", "FAILED"],
    )
    .await;
    rows.push(Reported {
        case: "a custom vocabulary's DownloadUri".to_string(),
        uri: reported(&vocabulary, "DownloadUri", "a custom vocabulary"),
        object: object(format!("{account}/vocabulary/served-vocabulary")),
        expected: Expected::Lines(phrases),
    });

    let filter = call(
        router,
        "GetVocabularyFilter",
        json!({"VocabularyFilterName": "served-filter"}),
    )
    .await;
    rows.push(Reported {
        case: "a vocabulary filter's DownloadUri".to_string(),
        uri: reported(
            filter.ok("a vocabulary filter's DownloadUri"),
            "DownloadUri",
            "a vocabulary filter",
        ),
        object: object(format!("{account}/vocabulary-filter/served-filter")),
        expected: Expected::Lines(words),
    });

    let medical = settled(
        router,
        "a custom medical vocabulary's DownloadUri",
        "GetMedicalVocabulary",
        json!({"VocabularyName": "served-medical-vocabulary"}),
        None,
        "VocabularyState",
        &["READY", "FAILED"],
    )
    .await;
    rows.push(Reported {
        case: "a custom medical vocabulary's DownloadUri".to_string(),
        uri: reported(&medical, "DownloadUri", "a custom medical vocabulary"),
        object: object(format!("{account}/medical-vocabulary/served-medical-vocabulary")),
        // The terms a table file's build loaded are its lines, so the file this service
        // builds from them holds the table's own lines.
        expected: Expected::Lines(
            String::from_utf8_lossy(TABLE)
                .lines()
                .map(str::to_string)
                .collect(),
        ),
    });

    let job = settled(
        router,
        "a transcription job's locations",
        "GetTranscriptionJob",
        json!({"TranscriptionJobName": "served-job"}),
        Some("TranscriptionJob"),
        "TranscriptionJobStatus",
        &["COMPLETED", "FAILED"],
    )
    .await;
    rows.push(Reported {
        case: "a transcription job's Transcript.TranscriptFileUri".to_string(),
        uri: reported(&job["Transcript"], "TranscriptFileUri", "a finished job"),
        object: object(format!("{account}/served-job.json")),
        expected: Expected::Transcript("served-job".to_string()),
    });
    rows.push(Reported {
        case: "a transcription job's Transcript.RedactedTranscriptFileUri".to_string(),
        uri: reported(
            &job["Transcript"],
            "RedactedTranscriptFileUri",
            "a finished job that asked for redaction",
        ),
        object: object(format!("{account}/redacted-served-job.json")),
        expected: Expected::Transcript("served-job".to_string()),
    });
    let subtitles = job["Subtitles"]["SubtitleFileUris"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("a job that asked for subtitles reports their locations"));
    // "If you specified both WebVTT and SubRip subtitle formats, two URIs are provided"
    // (model doc for `SubtitlesOutput$SubtitleFileUris`), in the order the request listed
    // the formats: a WebVTT file is identified by its own first line, and a SubRip file
    // with no cue is empty.
    for (index, (format, expected)) in [("vtt", "WEBVTT\n\n"), ("srt", "")].iter().enumerate() {
        let uri = subtitles
            .get(index)
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("the {format} subtitle location is reported"))
            .to_string();
        rows.push(Reported {
            case: format!("a transcription job's {format} SubtitleFileUris entry"),
            uri,
            object: object(format!("{account}/served-job.{format}")),
            expected: Expected::Text((*expected).to_string()),
        });
    }

    let call_analytics = settled(
        router,
        "a Call Analytics job's transcript location",
        "GetCallAnalyticsJob",
        json!({"CallAnalyticsJobName": "served-call"}),
        Some("CallAnalyticsJob"),
        "CallAnalyticsJobStatus",
        &["COMPLETED", "FAILED"],
    )
    .await;
    rows.push(Reported {
        case: "a Call Analytics job's Transcript.TranscriptFileUri".to_string(),
        uri: reported(
            &call_analytics["Transcript"],
            "TranscriptFileUri",
            "a finished Call Analytics job",
        ),
        object: object(format!("{account}/served-call.json")),
        expected: Expected::Transcript("served-call".to_string()),
    });

    rows
}

/// The location `member` of `holder` reports, or a panic naming what reported nothing.
fn reported(holder: &Value, member: &str, what: &str) -> String {
    holder[member]
        .as_str()
        .unwrap_or_else(|| panic!("{what} reports {member}, got {holder:?}"))
        .to_string()
}

/// Every location in this service's own store is a temporary pre-signed address on this
/// emulator's own endpoint, and a client carrying no credential of its own is served the
/// stored file from it.
#[tokio::test]
async fn a_service_managed_location_is_a_temporary_address_that_serves_its_file() {
    let router = emulator();
    given_staged_media(&router).await;
    let rows = service_managed_locations(&router).await;
    assert_eq!(
        rows.len(),
        8,
        "every member that reports a location in this service's own store is covered"
    );

    for row in &rows {
        let (case, uri) = (&row.case, &row.uri);
        // It leads back to THIS emulator, not to a host only AWS serves: no client of an
        // emulator resolves `s3.<region>.amazonaws.com`, so an address there could not be
        // fetched at all. The router here is not served on a port, so the endpoint it
        // names is the framework's base for an unserved emulator.
        assert!(
            uri.starts_with(&format!("{UNSERVED_URL_BASE}/")) && !uri.contains("amazonaws.com"),
            "case {case:?}: a reported location must address this emulator's own \
             endpoint, got {uri:?}"
        );
        assert_eq!(
            named_object(uri),
            row.object,
            "case {case:?}: the address must name this file's own object, got {uri:?}"
        );
        // It is TEMPORARY and pre-signed: the six SigV4 query fields, the window the
        // model documents, and the scope of the store that holds the file.
        for name in SIGNING_FIELDS {
            assert!(
                !field(uri, name).is_empty(),
                "case {case:?}: a temporary address carries {name}, got {uri:?}"
            );
        }
        assert_eq!(
            field(uri, "X-Amz-Algorithm"),
            SIGV4_ALGORITHM,
            "case {case:?}: expected a SigV4 signature, got {uri:?}"
        );
        assert_eq!(
            field(uri, "X-Amz-Expires"),
            SERVICE_MANAGED_VALIDITY_SECONDS.to_string(),
            "case {case:?}: a service-managed address is valid for the documented window, \
             got {uri:?}"
        );
        let scope = field(uri, "X-Amz-Credential").replace("%2F", "/");
        assert!(
            scope.contains(&format!("/{REGION}/s3/aws4_request")),
            "case {case:?}: the signature is scoped to the store that holds the file, got \
             {scope:?}"
        );
        assert!(
            uri.len() <= 2_000,
            "case {case:?}: a reported location is a modelled Uri (@length 1..2000), got \
             {} characters",
            uri.len()
        );

        assert_serves(&router, row).await;
    }

    // Reading again reports the same OBJECT: the address is temporary, so what two reads
    // agree on is which file it leads to — and both lead to the file.
    for row in &rows {
        let again = service_managed_locations_again(&router, row).await;
        assert_eq!(
            named_object(&again),
            row.object,
            "case {:?}: the object a reported location names is a function of the stored \
             record, got {again:?}",
            row.case
        );
        assert_serves(&router, &Reported {
            case: format!("{} (read again)", row.case),
            uri: again,
            object: row.object.clone(),
            expected: row.expected.clone(),
        })
        .await;
    }
}

/// The location the same reader reports for `row` on a SECOND read.
async fn service_managed_locations_again(router: &MultiServiceRouter, row: &Reported) -> String {
    let (operation, input, holder, member, index) = match row.case.as_str() {
        case if case.starts_with("a custom vocabulary") => (
            "GetVocabulary",
            json!({"VocabularyName": "served-vocabulary"}),
            None,
            "DownloadUri",
            None,
        ),
        case if case.starts_with("a vocabulary filter") => (
            "GetVocabularyFilter",
            json!({"VocabularyFilterName": "served-filter"}),
            None,
            "DownloadUri",
            None,
        ),
        case if case.starts_with("a custom medical vocabulary") => (
            "GetMedicalVocabulary",
            json!({"VocabularyName": "served-medical-vocabulary"}),
            None,
            "DownloadUri",
            None,
        ),
        case if case.contains("RedactedTranscriptFileUri") => (
            "GetTranscriptionJob",
            json!({"TranscriptionJobName": "served-job"}),
            Some(("TranscriptionJob", "Transcript")),
            "RedactedTranscriptFileUri",
            None,
        ),
        case if case.contains("SubtitleFileUris") => (
            "GetTranscriptionJob",
            json!({"TranscriptionJobName": "served-job"}),
            Some(("TranscriptionJob", "Subtitles")),
            "SubtitleFileUris",
            Some(usize::from(case.contains("srt"))),
        ),
        case if case.starts_with("a Call Analytics job") => (
            "GetCallAnalyticsJob",
            json!({"CallAnalyticsJobName": "served-call"}),
            Some(("CallAnalyticsJob", "Transcript")),
            "TranscriptFileUri",
            None,
        ),
        _ => (
            "GetTranscriptionJob",
            json!({"TranscriptionJobName": "served-job"}),
            Some(("TranscriptionJob", "Transcript")),
            "TranscriptFileUri",
            None,
        ),
    };
    let reply = call(router, operation, input).await;
    let body = reply.ok(&row.case).clone();
    let held = match holder {
        Some((job, member)) => body[job][member].clone(),
        None => body,
    };
    match index {
        Some(index) => held[member][index]
            .as_str()
            .unwrap_or_else(|| panic!("case {:?}: the location is reported again", row.case))
            .to_string(),
        None => reported(&held, member, &row.case),
    }
}

/// A location in a bucket the REQUEST named is reported as the plain address of that
/// object on the endpoint this emulator is reached on, with nothing signed for it:
/// the caller addresses its own bucket with its own credentials, so this service puts no
/// signature of its own on a bucket it does not own.
#[tokio::test]
async fn a_location_in_the_callers_own_bucket_is_reported_unsigned_on_this_endpoint() {
    let router = emulator();
    given_staged_media(&router).await;
    given_object(&router, CALLER_BUCKET, "placeholder", b"created by the caller").await;

    // Rows: the output forms a start may name for a bucket of its own — a bucket alone, a
    // key naming a folder, a key naming the output file — the Call Analytics family's own
    // single `OutputLocation`, and two whose request text IMITATES this service's own
    // store: a bucket named like the path-style Amazon S3 host of a Region, with a key
    // under it spelling that Region's service-managed bucket and another record's object.
    // WHOSE bucket a job's output is in follows from the stored request, so those two are
    // the caller's like any other and nothing is signed for them. Each row's last field is
    // the `BUCKET/KEY` of the object the location names.
    let own_store_bucket = service_managed_bucket();
    let account = engine::emulator_account_id();
    let rows: Vec<(&str, &str, Value, &str, &str, String)> = vec![
        (
            "a transcription job whose start named a bucket alone",
            "StartTranscriptionJob",
            json!({
                "TranscriptionJobName": "callers-bucket",
                "LanguageCode": "en-US",
                "Media": media(),
                "OutputBucketName": CALLER_BUCKET,
            }),
            "GetTranscriptionJob",
            "TranscriptionJob",
            format!("{CALLER_BUCKET}/callers-bucket.json"),
        ),
        (
            "a transcription job whose OutputKey names a folder",
            "StartTranscriptionJob",
            json!({
                "TranscriptionJobName": "callers-folder",
                "LanguageCode": "en-US",
                "Media": media(),
                "OutputBucketName": CALLER_BUCKET,
                "OutputKey": "transcripts/",
            }),
            "GetTranscriptionJob",
            "TranscriptionJob",
            format!("{CALLER_BUCKET}/transcripts/callers-folder.json"),
        ),
        (
            "a transcription job whose OutputKey names the output file",
            "StartTranscriptionJob",
            json!({
                "TranscriptionJobName": "callers-file",
                "LanguageCode": "en-US",
                "Media": media(),
                "OutputBucketName": CALLER_BUCKET,
                "OutputKey": "transcripts/chosen.json",
            }),
            "GetTranscriptionJob",
            "TranscriptionJob",
            format!("{CALLER_BUCKET}/transcripts/chosen.json"),
        ),
        (
            "a Call Analytics job whose start named an output location",
            "StartCallAnalyticsJob",
            json!({
                "CallAnalyticsJobName": "callers-call",
                "Media": media(),
                "OutputLocation": format!("s3://{CALLER_BUCKET}/calls/"),
            }),
            "GetCallAnalyticsJob",
            "CallAnalyticsJob",
            format!("{CALLER_BUCKET}/calls/callers-call.json"),
        ),
        (
            "a transcription job whose bucket imitates the path-style Amazon S3 host and \
             whose key imitates this service's own store",
            "StartTranscriptionJob",
            json!({
                "TranscriptionJobName": "callers-imitation",
                "LanguageCode": "en-US",
                "Media": media(),
                "OutputBucketName": format!("s3.{REGION}.amazonaws.com"),
                "OutputKey": format!("{own_store_bucket}/{account}/served-job.json"),
            }),
            "GetTranscriptionJob",
            "TranscriptionJob",
            format!(
                "s3.{REGION}.amazonaws.com/{own_store_bucket}/{account}/served-job.json"
            ),
        ),
        (
            "a Call Analytics job whose output location imitates this service's own store",
            "StartCallAnalyticsJob",
            json!({
                "CallAnalyticsJobName": "callers-imitating-call",
                "Media": media(),
                "OutputLocation": format!(
                    "s3://s3.{REGION}.amazonaws.com/{own_store_bucket}/{account}/served-call.json"
                ),
            }),
            "GetCallAnalyticsJob",
            "CallAnalyticsJob",
            format!(
                "s3.{REGION}.amazonaws.com/{own_store_bucket}/{account}/served-call.json"
            ),
        ),
    ];

    for (case, start, request, _, _, _) in &rows {
        call(&router, start, request.clone()).await.ok(case);
    }

    for (case, _, request, get, holder, expected) in &rows {
        let name_member = if *holder == "CallAnalyticsJob" {
            "CallAnalyticsJobName"
        } else {
            "TranscriptionJobName"
        };
        let status_member = if *holder == "CallAnalyticsJob" {
            "CallAnalyticsJobStatus"
        } else {
            "TranscriptionJobStatus"
        };
        let job = settled(
            &router,
            case,
            get,
            json!({name_member: request[name_member].clone()}),
            Some(holder),
            status_member,
            &["COMPLETED", "FAILED"],
        )
        .await;
        let reported = reported(&job["Transcript"], "TranscriptFileUri", case);
        assert_eq!(
            &reported,
            &format!("{UNSERVED_URL_BASE}/{expected}"),
            "case {case:?}: a location in the caller's own bucket is reported as that \
             object's address on this emulator's own endpoint, got {reported:?}"
        );
        assert_eq!(
            named_object(&reported),
            format!("/{expected}"),
            "case {case:?}: the address must name the caller's own object, got {reported:?}"
        );
        assert!(
            query_of(&reported).is_empty(),
            "case {case:?}: nothing is signed for a bucket the caller owns, got \
             {reported:?}"
        );
    }
}

/// An emulator with request authentication ON — the `--auth on` path the emulator binary
/// runs — together with the key pair it published for this run, which is the key its own
/// handlers sign a reported location with.
struct Authenticating {
    router: MultiServiceRouter,
    access_key_id: String,
    secret_access_key: String,
}

impl Authenticating {
    fn new() -> Self {
        let engine: std::sync::Arc<engine::InMemoryEngine> =
            std::sync::Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(true);
        let (access_key_id, secret_access_key) =
            match router.vended_credentials().into_iter().next() {
                Some(emulator_server::VendedCredentials::AwsAccessKey {
                    access_key_id,
                    secret_access_key,
                }) => (access_key_id, secret_access_key),
                other => panic!(
                    "with authentication on the emulator must vend a SigV4 key pair, got \
                     {other:?}"
                ),
            };
        router.register_service(
            service_com_amazonaws_transcribe_transcribe::EmulatedService::new(
                std::sync::Arc::clone(&engine),
            ),
        );
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            engine,
        ));
        Self {
            router,
            access_key_id,
            secret_access_key,
        }
    }

    /// One request this emulator VERIFIES, signed with the key pair it published through
    /// its own presigner — so what this suite drives is a request the emulator computes
    /// the same signature for, rather than a signature written here by hand.
    async fn signed(
        &self,
        method: &str,
        signing_name: &str,
        endpoint: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> (u16, Vec<u8>) {
        let target = emulator_server::request_authentication::PresignTarget {
            endpoint,
            method,
            signing_name,
            region: REGION,
            path,
            query: &[],
            headers,
        };
        let url = emulator_server::request_authentication::presign_url(
            &target,
            &self.access_key_id,
            &self.secret_access_key,
            SystemTime::now(),
            300,
        )
        .expect("the request must be signable");
        let host = endpoint.trim_start_matches("https://").to_string();
        let mut builder = http::Request::builder()
            .method(method)
            .uri(url)
            .header("host", host);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        // The header a presigned request to a service outside the S3 family must send for
        // its body to verify, which the presigner signed in.
        if let Some((name, value)) =
            emulator_server::request_authentication::presigned_payload_header(signing_name, method)
        {
            builder = builder.header(name, value);
        }
        let request = builder
            .body(bytes::Bytes::copy_from_slice(body))
            .expect("the request builds");
        let response = self.router.dispatch(request).await;
        (
            response.status().as_u16(),
            response.body().as_ref().to_vec(),
        )
    }

    /// One transcribe call in the service's own wire envelope, verified.
    async fn call(&self, case: &str, operation: &str, input: Value) -> Value {
        let target = format!("Transcribe.{operation}");
        let (status, body) = self
            .signed(
                "POST",
                "transcribe",
                "https://transcribe.us-east-1.amazonaws.com",
                "/",
                &[
                    ("x-amz-target", target.as_str()),
                    ("content-type", "application/x-amz-json-1.1"),
                ],
                &serde_json::to_vec(&input).expect("serializable input"),
            )
            .await;
        assert_eq!(
            status,
            200,
            "case {case:?}: {operation} must be served to a verified caller, got HTTP \
             {status} with body {:?}",
            String::from_utf8_lossy(&body)
        );
        serde_json::from_slice(&body).unwrap_or_else(|error| {
            panic!(
                "case {case:?}: {operation} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(&body)
            )
        })
    }

    /// Poll a vocabulary until its build has finished, and answer the reader's answer.
    async fn vocabulary_settled(&self, case: &str, name: &str) -> Value {
        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            let read = self
                .call(case, "GetVocabulary", json!({"VocabularyName": name}))
                .await;
            if read["VocabularyState"] != json!("PENDING") {
                return read;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the build of {name:?} never finished"
            );
            std::thread::sleep(POLL_INTERVAL);
        }
    }
}

/// The SIGNATURE on a reported location is what authorises the fetch: with request
/// authentication on, the address as reported is served, while the same object addressed
/// without that signature — or with another — is refused.
///
/// This is the rule the reported location exists for: a client follows it with no
/// credential of its own, so an address carrying no signature is a request by nobody,
/// which the store refuses.
#[tokio::test]
async fn the_signature_on_a_reported_location_is_what_authorises_the_fetch() {
    let emulator = Authenticating::new();
    let case = "a custom vocabulary's DownloadUri, on an authenticating emulator";
    let phrases = vec!["Alderaan".to_string()];
    emulator
        .call(
            case,
            "CreateVocabulary",
            json!({
                "VocabularyName": "authenticated-vocabulary",
                "LanguageCode": "en-US",
                "Phrases": phrases.clone(),
            }),
        )
        .await;
    let read = emulator
        .vocabulary_settled(case, "authenticated-vocabulary")
        .await;
    assert_eq!(
        read["VocabularyState"],
        json!("READY"),
        "case {case:?}: a vocabulary built from its request's own phrases must settle \
         READY, got {read:?}"
    );
    let uri = reported(&read, "DownloadUri", "a built custom vocabulary");

    // The address as reported: served, with the terms the build stored.
    let (status, body) = fetch(&emulator.router, &uri).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the address as reported must serve the file, got HTTP {status} \
         with body {:?}",
        String::from_utf8_lossy(&body)
    );
    let served: Vec<String> = String::from_utf8_lossy(&body)
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(
        served, phrases,
        "case {case:?}: expected the stored terms {phrases:?}, got {served:?}"
    );

    // The same object, addressed without the signature the location carries or with
    // another: not served. Each row is one way an address stops being the one this
    // service signed.
    let flipped = {
        let signature = field(&uri, "X-Amz-Signature");
        let mut digits = signature.chars();
        let first = digits.next().unwrap_or('0');
        let other = if first == '0' { '1' } else { '0' };
        uri.replace(
            &signature,
            &format!("{other}{}", digits.collect::<String>()),
        )
    };
    let rows: Vec<(&str, String)> = vec![
        ("no signature at all", named_object(&uri).to_string()),
        ("another signature", flipped),
        (
            "a window the client widened",
            uri.replace(
                &format!("X-Amz-Expires={SERVICE_MANAGED_VALIDITY_SECONDS}"),
                "X-Amz-Expires=604800",
            ),
        ),
    ];
    for (row, address) in &rows {
        let address = match address.starts_with("https://") {
            true => address.clone(),
            false => format!("https://{}{address}", host_of(&uri)),
        };
        let (status, body) = fetch(&emulator.router, &address).await;
        assert_ne!(
            status, 200,
            "case {row:?}: an address this service did not sign must not serve the file, \
             got HTTP {status} with body {:?}",
            String::from_utf8_lossy(&body)
        );
    }
}

/// The host a reported address names.
fn host_of(uri: &str) -> String {
    uri.split_once("://")
        .and_then(|(_scheme, rest)| rest.split('/').next())
        .unwrap_or_default()
        .to_string()
}
