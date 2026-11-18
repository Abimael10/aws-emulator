// SPDX-License-Identifier: CC0-1.0

//! WHICH ENDPOINT a fetchable location this service reports is published on — for every
//! endpoint this emulator can be reached on, not for one of them.
//!
//! THE RULE THESE ROWS ASSERT: a location this service reports for a file in its own
//! service-managed store is published on the endpoint THIS EMULATOR is reached on, so a
//! client that follows the address it was just handed arrives back at the emulator that
//! minted it and is served the stored file. The model states the contract for one such
//! location — "If you didn't include `OutputBucketName` in your transcription job request,
//! your transcript is stored in a service-managed bucket, and `TranscriptFileUri` provides
//! you with a temporary URI you can use for secure access to your transcript"
//! (`smithy_query(query="doc", shape="Transcript", member="TranscriptFileUri")`) — and a
//! `DownloadUri` is the same kind of location ("use this URI to view or download the custom
//! vocabulary", `smithy_query(query="operation", operation="GetVocabulary")`). An address on
//! a host only the real Amazon S3 serves satisfies neither: a client of an emulator never
//! resolves it, and the account it names holds no such bucket, so the file at the location
//! could not be read however exactly its bucket and key were spelled.
//!
//! The rows differ along what the rule generalises over: the ENDPOINT the emulator is served
//! on (a loopback address and port, that host on another port, a named host with a port, a
//! named host with no port at all, an IPv6 literal) and which reported member carries the
//! location (a custom vocabulary's `DownloadUri`, a vocabulary filter's, and a transcription
//! job's `Transcript.TranscriptFileUri`), with a text file of terms or a transcript document
//! at the address. The three records are built ONCE and read back under each endpoint in
//! turn, so what is pinned is that one stored object is announced at whatever address this
//! emulator answers on, while the object the address names never moves.
//!
//! This is the WHERE half; the suite beside it (`crate::served_download_locations`) is the
//! WHAT half — which member reports a location at all, whose bucket it is in, what document
//! sits there, and that an address carrying no signature of this service's is not served.
//! Both read a reported address through that suite's one request helper, so neither can
//! follow a location in a way the other would not.

use serde_json::{json, Value};
use smithy_server_runtime::EndpointUrl;

use crate::media_object_resolution::{BUCKET, STAGED_KEYS};
use crate::served_download_locations::{fetch, named_object, service_managed_bucket};
use crate::transcription_jobs::Transcribe;

/// The Region every call of this suite is signed for, and so the Region whose
/// service-managed bucket these files live in. The same Region
/// `crate::served_download_locations` serves, whose spelling of that bucket this suite
/// reuses ([`service_managed_bucket`]).
const REGION: &str = "us-east-1";

/// The endpoints the rows serve this emulator on, each a base URL a data port can publish
/// (`scheme://host[:port]`, which is the only form an endpoint takes).
const ENDPOINTS: &[&str] = &[
    // The loopback address and port the emulator binary publishes by default.
    "https://127.0.0.1:9297",
    // The same host on another port: the port is part of the address a client dials.
    "https://127.0.0.1:34567",
    // A named host carrying a port of its own.
    "https://emulator.test:8443",
    // A named host with NO port, so the address carries the https default.
    "https://emulator.test",
    // An IPv6 loopback literal, which an address names in brackets.
    "https://[::1]:9297",
];

/// The custom vocabulary, the vocabulary filter and the transcription job the rows read
/// their locations back from. Each name matches the `^[0-9a-zA-Z._-]+$` pattern its member
/// declares.
const VOCABULARY: &str = "endpoint-reported-vocabulary";
/// See [`VOCABULARY`].
const FILTER: &str = "endpoint-reported-filter";
/// See [`VOCABULARY`].
const JOB: &str = "endpoint-reported-job";

/// The `Phrases` the custom vocabulary is built from, which are the lines of the file its
/// `DownloadUri` names.
fn phrases() -> Vec<String> {
    vec!["Alderaan".to_string(), "Tatooine".to_string()]
}

/// The `Words` the vocabulary filter is built from, which are the lines of the file its own
/// `DownloadUri` names.
fn words() -> Vec<String> {
    vec!["dang".to_string(), "heck".to_string()]
}

/// What is served at a reported location.
enum Served {
    /// The text file a vocabulary family's build wrote: one term per line, in the order the
    /// request listed them.
    Terms(Vec<String>),
    /// The transcript document of the job named here.
    TranscriptOf(&'static str),
}

/// One reported location: what reports it, the address as reported, the object that address
/// must name and what is served there.
struct Reported {
    case: String,
    uri: String,
    /// The object the address names, relative to the store: `/<bucket>/<key>`.
    object: String,
    served: Served,
}

/// Serve the emulator behind `harness` on `endpoint`, as starting its data port there does.
///
/// The base is shared by every clone of one router (`MultiServiceRouter::set_url_base`:
/// "Shared by every clone"), so setting it through a clone sets it for the harness's own
/// router, which is the one that answers the calls below.
fn served_at(harness: &Transcribe, endpoint: &str) {
    let base = EndpointUrl::parse(endpoint)
        .unwrap_or_else(|| panic!("case {endpoint:?}: a data port publishes a bare https URL"));
    let mut router = (*harness.router).clone();
    router.set_url_base(base);
}

/// Read every location the three records report, through the service's own readers, while
/// the emulator is served on `endpoint`.
async fn reported(harness: &Transcribe, endpoint: &str) -> Vec<Reported> {
    let account = engine::emulator_account_id();
    let bucket = service_managed_bucket();
    let region = Some(REGION);

    let vocabulary_case = format!("a custom vocabulary's DownloadUri, served on {endpoint}");
    let vocabulary = harness
        .call_in_region("GetVocabulary", json!({"VocabularyName": VOCABULARY}), region)
        .await;
    let filter_case = format!("a vocabulary filter's DownloadUri, served on {endpoint}");
    let filter = harness
        .call_in_region(
            "GetVocabularyFilter",
            json!({"VocabularyFilterName": FILTER}),
            region,
        )
        .await;
    let job_case = format!("a transcription job's TranscriptFileUri, served on {endpoint}");
    let job = harness
        .call_in_region(
            "GetTranscriptionJob",
            json!({"TranscriptionJobName": JOB}),
            region,
        )
        .await;
    let transcript = job.ok(&job_case)["TranscriptionJob"]["Transcript"]["TranscriptFileUri"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {job_case:?}: a COMPLETED job reports where its transcript is, got {:?}",
                job.body
            )
        })
        .to_string();

    vec![
        Reported {
            uri: vocabulary
                .ok(&vocabulary_case)
                .get("DownloadUri")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!(
                        "case {vocabulary_case:?}: a READY vocabulary reports a DownloadUri, \
                         got {:?}",
                        vocabulary.body
                    )
                })
                .to_string(),
            case: vocabulary_case,
            object: format!("/{bucket}/{account}/vocabulary/{VOCABULARY}"),
            served: Served::Terms(phrases()),
        },
        Reported {
            uri: filter.member(&filter_case, "DownloadUri"),
            case: filter_case,
            object: format!("/{bucket}/{account}/vocabulary-filter/{FILTER}"),
            served: Served::Terms(words()),
        },
        Reported {
            case: job_case,
            uri: transcript,
            object: format!("/{bucket}/{account}/{JOB}.json"),
            served: Served::TranscriptOf(JOB),
        },
    ]
}

/// Every location this service reports for a file of its own store is published on the
/// endpoint this emulator is served on, whatever that endpoint is, and a client that follows
/// it with no credential of its own is served the stored file.
#[tokio::test]
async fn a_reported_location_is_published_on_the_endpoint_the_emulator_is_served_on() {
    let harness = Transcribe::with_object_store();
    // The media the job is pointed at, staged in the emulator's own object store first, as
    // a caller uploads it before naming it.
    let media = format!("s3://{BUCKET}/{}", STAGED_KEYS[0]);
    harness.given_media_objects(&[media.as_str()]).await;

    harness
        .call_in_region(
            "CreateVocabulary",
            json!({
                "VocabularyName": VOCABULARY,
                "LanguageCode": "en-US",
                "Phrases": phrases(),
            }),
            Some(REGION),
        )
        .await
        .ok("a custom vocabulary built from its request's own phrases");
    harness
        .call_in_region(
            "CreateVocabularyFilter",
            json!({
                "VocabularyFilterName": FILTER,
                "LanguageCode": "en-US",
                "Words": words(),
            }),
            Some(REGION),
        )
        .await
        .ok("a vocabulary filter built from its request's own words");
    harness
        .call_in_region(
            "StartTranscriptionJob",
            json!({
                "TranscriptionJobName": JOB,
                "LanguageCode": "en-US",
                // No OutputBucketName: the transcript is this service's own to store, and
                // so its location is this service's own to publish.
                "Media": {"MediaFileUri": media},
            }),
            Some(REGION),
        )
        .await
        .ok("a transcription job whose output is this service's own");

    // Waited out through the harness's own poll loops, which look again until the build and
    // the job have settled: each row below reads a record whose file this service has
    // already written.
    harness.vocabulary_ready(VOCABULARY, Some(REGION)).await;
    let job = harness.transcription_job_settled(JOB, Some(REGION)).await;
    assert_eq!(
        job["TranscriptionJobStatus"],
        json!("COMPLETED"),
        "a job over media the object store holds must complete, got {job:?}"
    );

    for endpoint in ENDPOINTS {
        served_at(&harness, endpoint);
        let rows = reported(&harness, endpoint).await;
        for row in &rows {
            let (case, uri) = (&row.case, &row.uri);
            // It leads back to THIS emulator, at the address this one is reached on —
            // never to a host only the real Amazon S3 serves, which no client of an
            // emulator can fetch from at all.
            assert!(
                uri.starts_with(&format!("{endpoint}/")),
                "case {case:?}: a reported location must be published on this emulator's own \
                 endpoint {endpoint:?}, got {uri:?}"
            );
            assert!(
                !uri.contains("amazonaws.com"),
                "case {case:?}: a reported location must not address a host only AWS serves, \
                 got {uri:?}"
            );
            // The endpoint moved; the object the address names did not.
            assert_eq!(
                named_object(uri),
                row.object,
                "case {case:?}: the address must name this file's own object, got {uri:?}"
            );

            let (status, body) = fetch(&harness.router, uri).await;
            assert_eq!(
                status, 200,
                "case {case:?}: a client that follows {uri:?} with no credential of its own \
                 must be served the stored file, got HTTP {status} with body {:?}",
                String::from_utf8_lossy(&body)
            );
            let text = String::from_utf8_lossy(&body).to_string();
            match &row.served {
                Served::Terms(expected) => {
                    let served: Vec<String> = text.lines().map(str::to_string).collect();
                    assert_eq!(
                        &served, expected,
                        "case {case:?}: expected the file at {uri:?} to hold {expected:?}, got \
                         {served:?}"
                    );
                }
                Served::TranscriptOf(job) => {
                    let document: Value = serde_json::from_str(&text).unwrap_or_else(|error| {
                        panic!(
                            "case {case:?}: the transcript document must be JSON ({error}): \
                             {text:?}"
                        )
                    });
                    assert_eq!(
                        document["jobName"],
                        json!(job),
                        "case {case:?}: the transcript document names its job, got {document:?}"
                    );
                    assert!(
                        document["results"]["transcripts"][0]["transcript"].is_string(),
                        "case {case:?}: the transcript document carries a results envelope, got \
                         {document:?}"
                    );
                }
            }
        }
    }
}
