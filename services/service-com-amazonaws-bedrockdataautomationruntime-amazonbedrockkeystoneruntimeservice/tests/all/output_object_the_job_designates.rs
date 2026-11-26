// SPDX-License-Identifier: CC0-1.0

//! Following a finished data-automation job the only way its API allows: poll it to a
//! terminal status, read the output location it REPORTS, and fetch the standard-output
//! document that location's index DESIGNATES.
//!
//! THE RULE: every object a successful asynchronous data-automation job designates — the
//! index at the location `GetDataAutomationStatus` reports, and the per-asset standard
//! output that index names — is in the object store and readable with 200 by the time any
//! read reports the job terminal, carrying the extracted-document shape and the ordinary
//! stored-object metadata a service's write leaves, and it is there for a consumer that
//! walks the output prefix as well as for one that follows the report. A key under the job's
//! own segment that the job designates NOTHING for is genuinely absent, so the object
//! store's `NoSuchKey` for it is the right answer.
//!
//! Nothing here constructs the key it reads: every address is taken from what the job itself
//! answered, so this suite asserts the DESIGNATION — that no address a client can only learn
//! from the job leads nowhere. What the designated addresses resolve to (the key layout, the
//! body members of both documents, the per-modality content) is `job_standard_output_objects`,
//! and what reading them reports about their bytes is `job_output_object_integrity`.
//!
//! The rows differ along what the rule generalises over: the asset's FORMAT and so its file
//! type (PDF, UTF-8 text), how many PAGES it holds (one and three), the shape of the
//! submitted output prefix (a deep prefix, one written with a trailing separator, the bucket
//! root) and how deep the input asset's own key is.
//!
//! Amazon S3 is mounted beside this service on one engine — what `core/router` assembles —
//! because the whole rule is about objects, and they are read back over Amazon S3's own wire
//! protocol through the harness every suite about a job's output shares
//! (`crate::support`).

use std::sync::Arc;

use serde_json::json;
use service_com_amazonaws_bedrockdataautomationruntime_amazonbedrockkeystoneruntimeservice as bda;
use service_com_amazonaws_s3_amazons3 as s3;
use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    call, create_bucket, poll_until_terminal, put_object, read_object, s3_call,
    DEFAULT_MEDIA_TYPE, FULL_OBJECT, SSE_S3,
};

/// The data-automation project every submission names, arranged in the store of the service
/// that owns it: a submission naming a project nobody created is refused before any rule
/// here could be reached.
const PROJECT_ARN: &str = "arn:aws:bedrock:us-west-2:000000000000:data-automation-project/ledgers";

/// The bucket the input assets are staged in.
const ASSET_BUCKET: &str = "bda-designated-input";

/// The bucket every job below writes its output into.
const OUTPUT_BUCKET: &str = "bda-designated-output";

/// The tail of a per-asset standard-output document's key: the subdocument segment of the
/// asset segment, and the document itself.
const STANDARD_OUTPUT_TAIL: &str = "/standard_output/0/result.json";

/// One emulator: this service and Amazon S3 on a single shared engine behind the production
/// router, with the referenced project arranged and both buckets created.
async fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    crate::support::seed_project(&engine, PROJECT_ARN, "data-automation-ledgers");
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(bda::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(s3::EmulatedService::new(engine));
    for bucket in [ASSET_BUCKET, OUTPUT_BUCKET] {
        create_bucket(&router, bucket).await;
    }
    router
}

/// List what `bucket` holds under `prefix`, the way a consumer of a job's output prefix does.
///
/// The prefix travels as a query-parameter value, so it is percent-encoded by the crate that
/// implements that encoding rather than interpolated raw: a key separator inside it is part of
/// the VALUE.
async fn list_prefix(
    router: &MultiServiceRouter,
    bucket: &str,
    prefix: &str,
) -> crate::support::ObjectAnswer {
    let encoded =
        percent_encoding::utf8_percent_encode(prefix, percent_encoding::NON_ALPHANUMERIC);
    s3_call(
        router,
        "GET",
        &format!("/{bucket}?list-type=2&prefix={encoded}"),
        &[],
        &[],
    )
    .await
}

/// The bucket and key an `s3://` URI addresses — the one place this suite takes an address
/// apart, because every address it reads comes from the job rather than from the test.
fn s3_address(s3_uri: &str) -> (String, String) {
    let tail = s3_uri
        .strip_prefix("s3://")
        .unwrap_or_else(|| panic!("an s3:// URI, got {s3_uri:?}"));
    let (bucket, key) = tail
        .split_once('/')
        .unwrap_or_else(|| panic!("an s3:// URI naming an object, got {s3_uri:?}"));
    (bucket.to_string(), key.to_string())
}

/// Submit one invocation reading `input_s3_uri` and writing under `output_s3_uri`, and answer
/// its `invocationArn`.
async fn submit(
    router: &MultiServiceRouter,
    case: &str,
    input_s3_uri: &str,
    output_s3_uri: &str,
) -> String {
    let answer = call(
        router,
        "InvokeDataAutomationAsync",
        json!({
            "inputConfiguration": {"s3Uri": input_s3_uri},
            "outputConfiguration": {"s3Uri": output_s3_uri},
            "dataAutomationProfileArn": crate::support::PROFILE_ARN,
            "dataAutomationConfiguration": {"dataAutomationProjectArn": PROJECT_ARN},
        }),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected 200 from InvokeDataAutomationAsync, got {} / {}",
        answer.status, answer.body
    );
    answer.body["invocationArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no invocationArn in {}", answer.body))
        .to_string()
}

/// Run one job to its terminal status and answer the standard-output address its own reported
/// index DESIGNATES, together with the index's own address: the two addresses a client can
/// only learn from the job.
async fn designated_addresses(
    router: &MultiServiceRouter,
    case: &str,
    invocation_arn: &str,
) -> (String, String) {
    let status = poll_until_terminal(router, case, invocation_arn).await;
    assert_eq!(
        status["status"].as_str(),
        Some("Success"),
        "case {case:?}: a job whose asset the store holds must succeed, got {status}"
    );

    // STEP 1: the location the job itself reports. A client has nothing else to go on.
    let reported = status["outputConfiguration"]["s3Uri"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no reported output location in {status}"))
        .to_string();
    let (index_bucket, index_key) = s3_address(&reported);
    let index = read_object(router, &index_bucket, &index_key).await;
    assert_eq!(
        index.status, 200,
        "case {case:?}: the location the job reports ({reported:?}) must be readable, got {} / {}",
        index.status,
        index.text()
    );

    // STEP 2: the standard output that index designates — never a key this suite built.
    let index_body = index.json();
    let designated = index_body["output_metadata"][0]["segment_metadata"][0]
        ["standard_output_path"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the index designates no standard output: {index_body}")
        })
        .to_string();
    (designated, reported)
}

/// One row: the asset a job reads, where its output was asked for, and what the document the
/// job designates must then report about the asset it was read from.
struct Case {
    /// The key the asset is staged under.
    key: &'static str,
    /// The asset's bytes.
    bytes: Vec<u8>,
    /// The `outputConfiguration.s3Uri` the submission names.
    output_s3_uri: String,
    /// That prefix as a key segment, with no trailing separator — empty for a submission
    /// that named the bucket root.
    output_prefix: &'static str,
    /// The `file_type` the designated document must report.
    file_type: &'static str,
    /// How many pages it must report.
    pages: usize,
}

#[tokio::test]
async fn the_standard_output_a_finished_job_designates_reads_back() {
    let router = emulator().await;

    let rows: &[(&str, Case)] = &[
        (
            "a one-page PDF, deep output prefix, nested asset key",
            Case {
                key: "ledgers/2026/january.pdf",
                bytes: crate::support::pdf_document(&[&[
                    "Ledger entry 118 for Kestrel Foundry",
                    "Balance carried 640 USD",
                ]]),
                output_s3_uri: format!("s3://{OUTPUT_BUCKET}/extractions/ledgers/2026"),
                output_prefix: "extractions/ledgers/2026",
                file_type: "PDF",
                pages: 1,
            },
        ),
        (
            "a three-page PDF, output prefix written with a trailing separator",
            Case {
                key: "audit.pdf",
                bytes: crate::support::pdf_document(&[
                    &["Audit page one", "Opening position 12 USD"],
                    &["Audit page two", "Adjustment 30 USD"],
                    &["Audit page three", "Closing position 42 USD"],
                ]),
                output_s3_uri: format!("s3://{OUTPUT_BUCKET}/audits/"),
                output_prefix: "audits",
                file_type: "PDF",
                pages: 3,
            },
        ),
        (
            "a UTF-8 text asset, output asked for at the bucket root",
            Case {
                key: "handover/notes.txt",
                bytes: b"Handover 9041\nCountersigned by Grace Hopper\n".to_vec(),
                output_s3_uri: format!("s3://{OUTPUT_BUCKET}"),
                output_prefix: "",
                file_type: "TXT",
                pages: 1,
            },
        ),
    ];

    for (case, row) in rows {
        put_object(&router, case, ASSET_BUCKET, row.key, &row.bytes).await;
        let arn = submit(
            &router,
            case,
            &format!("s3://{ASSET_BUCKET}/{}", row.key),
            &row.output_s3_uri,
        )
        .await;
        let id = crate::support::invocation_id(&arn).to_string();

        let (designated, reported) = designated_addresses(&router, case, &arn).await;
        let (bucket, key) = s3_address(&designated);
        let (_, index_key) = s3_address(&reported);

        // The designated address is this job's own: inside the submitted prefix, under a
        // segment of the job's id, naming one subdocument's document.
        let segment = if row.output_prefix.is_empty() {
            id.clone()
        } else {
            format!("{}/{id}", row.output_prefix)
        };
        assert!(
            key.starts_with(&format!("{segment}/")) && key.ends_with(STANDARD_OUTPUT_TAIL),
            "case {case:?}: the designated standard output must live under this job's own \
             segment {segment:?} of the submitted prefix and name one subdocument's document, \
             got {designated:?}"
        );

        // And it answers, with the stored-object metadata a service's write leaves: the
        // store's own default media type (the job names none), the checksum the write asked
        // for over the whole object, the bucket's default encryption, and the identity and
        // range members every stored object carries (their values are per-run).
        let result = read_object(&router, &bucket, &key).await;
        assert_eq!(
            result.status, 200,
            "case {case:?}: the standard output the job designates ({designated:?}) must be \
             readable as soon as the job reports Success, got {} / {}",
            result.status,
            result.text()
        );
        for (header, want) in [
            ("content-type", DEFAULT_MEDIA_TYPE),
            ("accept-ranges", "bytes"),
            ("x-amz-checksum-type", FULL_OBJECT),
            ("x-amz-server-side-encryption", SSE_S3),
        ] {
            assert_eq!(
                result.header(header).as_deref(),
                Some(want),
                "case {case:?}: expected {header} {want:?} on the designated document, got {:?}",
                result.header(header)
            );
        }
        for header in ["x-amz-checksum-crc32", "etag", "last-modified"] {
            assert!(
                result.header(header).is_some_and(|value| !value.is_empty()),
                "case {case:?}: expected {header} present on the designated document, got {:?}",
                result.header(header)
            );
        }

        // The document the designated address serves is this job's own extraction: it names
        // the asset THIS submission pointed at, with that asset's own type and page range.
        let metadata = &result.json()["metadata"];
        for (member, want) in [
            ("s3_bucket", json!(ASSET_BUCKET)),
            ("s3_key", json!(row.key)),
            ("file_type", json!(row.file_type)),
            ("number_of_pages", json!(row.pages)),
        ] {
            assert_eq!(
                metadata[member], want,
                "case {case:?}: expected metadata.{member} {want} in the designated document, \
                 got {metadata}"
            );
        }

        // A consumer that walks the output prefix instead of following the report finds both
        // designated objects there.
        let listing = list_prefix(&router, &bucket, &segment).await;
        assert_eq!(
            listing.status, 200,
            "case {case:?}: the output prefix must list, got {} / {}",
            listing.status,
            listing.text()
        );
        let listed = listing.text();
        for listed_key in [&key, &index_key] {
            assert!(
                listed.contains(&format!("<Key>{listed_key}</Key>")),
                "case {case:?}: expected the listing of {segment:?} to hold {listed_key:?}, got \
                 {listed}"
            );
        }
    }
}

#[tokio::test]
async fn a_key_under_the_job_segment_the_job_designates_nothing_for_is_absent() {
    // The other half of the rule: what the job designates is there, and nothing else is
    // invented around it. One asset processed whole is ONE logical subdocument, so a read of
    // a second subdocument's document — a key with the designated one's own shape — is a
    // real absence, and `NoSuchKey` is the honest answer to it.
    let router = emulator().await;
    let case = "a second subdocument of a one-subdocument job";

    let key = "ledgers/february.pdf";
    put_object(
        &router,
        case,
        ASSET_BUCKET,
        key,
        &crate::support::pdf_document(&[&["Ledger entry 119", "Balance carried 700 USD"]]),
    )
    .await;
    let arn = submit(
        &router,
        case,
        &format!("s3://{ASSET_BUCKET}/{key}"),
        &format!("s3://{OUTPUT_BUCKET}/extractions"),
    )
    .await;

    let (designated, _) = designated_addresses(&router, case, &arn).await;
    let (bucket, designated_key) = s3_address(&designated);
    assert_eq!(
        read_object(&router, &bucket, &designated_key).await.status,
        200,
        "case {case:?}: the designated document must read back"
    );

    let absent_key = designated_key.replace(STANDARD_OUTPUT_TAIL, "/standard_output/1/result.json");
    let absent = read_object(&router, &bucket, &absent_key).await;
    assert_eq!(
        absent.status, 404,
        "case {case:?}: a subdocument the job designates nothing for must be absent, got {} / {}",
        absent.status,
        absent.text()
    );
    let served = absent.text();
    assert!(
        served.contains("<Code>NoSuchKey</Code>"),
        "case {case:?}: expected NoSuchKey for {absent_key:?}, got {served}"
    );
}
