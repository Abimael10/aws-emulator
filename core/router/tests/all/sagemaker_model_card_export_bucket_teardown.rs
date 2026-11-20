// SPDX-License-Identifier: CC0-1.0

//! Tearing down the bucket a MODEL CARD EXPORT wrote into: the document the job saved is
//! held under exactly the key the job REPORTED, so a client that deletes the artifact it
//! was handed empties the bucket and the `DeleteBucket` that follows succeeds.
//!
//! RULE: an object an emulated service writes into the caller's bucket on a job's behalf is
//! stored under a key composed only of what the API named the caller — here the request's
//! own `OutputConfig.S3OutputPath`, the model card as its own path segment and the export
//! job as the file stem, the layout the job's own `ModelCardExportJobArn`
//! (`model-card/<card>/export-job/<job>`) is built from — and the key the response reports
//! in `ExportArtifacts.S3ExportArtifacts` ("The Amazon S3 URI of the exported model
//! artifacts") is the key the object is stored under. So nothing is left in the bucket
//! under a key no response named: a client that deletes every URI it was handed, or every
//! key a listing reports, has emptied the bucket, and `DeleteBucket` answers 204 — while a
//! bucket that still holds a document is refused `BucketNotEmpty` (409), which is the rule
//! `DeleteBucket` states ("All objects (including all object versions and delete markers)
//! in the bucket must be deleted before the bucket itself can be deleted").
//!
//! Two crates have to agree for that to hold (SageMaker saves the document, Amazon S3
//! holds, lists and deletes it), so the suite lives here rather than in either of them.
//!
//! The rows differ along what the rule generalises over: WHERE the documents land (a nested
//! prefix, a prefix written with no trailing delimiter, the bucket root), HOW MANY exports
//! the bucket holds (one, two jobs of one card, jobs of two different cards), HOW MANY
//! versions the exported card holds and whether the request pins one, whether the client
//! wrote objects of its own beside them, and HOW the client empties the bucket (deleting
//! exactly the artifact URIs it was handed, or every key the listing reported).
//!
//! Nothing here asserts a value this emulator invented: each artifact is checked against
//! the key the REQUEST's own values compose, and the keys the teardown deletes are the ones
//! the service reported or the listing named.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, elements, form_encode, opt_element};

/// The Region every call here is signed for and addressed in.
const REGION: &str = "us-east-1";

/// How long a row waits for SageMaker to report an export job terminal, and how often it
/// looks while waiting. The budget is generous next to the span the job's terminal status
/// falls due after, so a loaded machine has room.
const PATIENCE: Duration = Duration::from_secs(120);
const LOOK: Duration = Duration::from_millis(25);

/// How many keys one page of the teardown's listing asks for: small enough that a row
/// holding more than a couple of objects really pages, which is what makes the continuation
/// token the teardown follows part of what is exercised.
const PAGE_KEYS: usize = 2;

/// How many pages of a listing the teardown reads before the listing is not ending: a page
/// holds [`PAGE_KEYS`] keys and no row writes more than a handful of objects, so this is
/// generous, and it is what makes a listing that keeps handing out the same token a report
/// rather than a suite that never returns.
const LISTING_PAGES: usize = 20;

/// The code Amazon S3 refuses the delete of a bucket that still holds something with, and
/// the code it answers a request naming a bucket it does not hold with. Written out, not
/// read off the service's own constants: both are fixed by the Amazon S3 API, so a literal
/// here is what catches a wrong wire code.
const BUCKET_NOT_EMPTY: &str = "BucketNotEmpty";
const NO_SUCH_BUCKET: &str = "NoSuchBucket";

/// The first bytes of every PDF document, so what the export left behind is a document a
/// reader opens rather than bytes shaped like one.
const PDF_HEADER: &[u8] = b"%PDF-";

/// Amazon S3 and SageMaker over ONE shared engine, as `core/router/src/lib.rs` mounts them
/// — so the document an export saves lands in this very object store.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_sagemaker_sagemaker::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header scoped to `service` in [`REGION`], which is what routes
/// the request and what a service reads the caller's Region from. The key id and signature
/// are not credentials — this suite turns request authentication off, so nothing is
/// verified.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One raw request to the emulator's Amazon S3 front door: path-style, so the bucket and
/// the key live in the URI and the body is the payload.
async fn s3(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: &[u8],
) -> (u16, Vec<u8>) {
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("authorization", authorization("s3"))
        .header("content-type", "application/xml")
        .body(Bytes::copy_from_slice(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, response.into_body().as_ref().to_vec())
}

/// The same request, with the answer read as the text an Amazon S3 payload is.
async fn s3_text(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: &[u8],
) -> (u16, String) {
    let (status, answered) = s3(router, method, path, body).await;
    (status, String::from_utf8_lossy(&answered).to_string())
}

/// One `awsJson1_1` call to SageMaker, under the target prefix its model names.
async fn sagemaker(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("api.sagemaker.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("SageMaker.{operation}"))
        .header("authorization", authorization("sagemaker"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One SageMaker call that must be served.
async fn sagemaker_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, answered) = sagemaker(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must be served, got {status} with {answered:?}"
    );
    answered
}

/// `PUT /<bucket>` — `CreateBucket`.
async fn given_bucket(router: &MultiServiceRouter, case: &str, bucket: &str) {
    let (status, body) = s3_text(router, "PUT", &format!("/{bucket}"), &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the bucket {bucket:?} must be created, got {status} with {body:?}"
    );
}

/// `PUT /<bucket>/<key>` — the client's own object write.
async fn put_object(router: &MultiServiceRouter, case: &str, bucket: &str, key: &str) {
    let payload = format!("the bytes of {key}");
    let (status, body) =
        s3_text(router, "PUT", &format!("/{bucket}/{key}"), payload.as_bytes()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the write of {key:?} must be accepted, got {status} with {body:?}"
    );
}

/// Every key `bucket` holds, read through `ListObjectsV2` the way a teardown reads it:
/// following the listing's own continuation token until the last page.
async fn listed_keys(router: &MultiServiceRouter, case: &str, bucket: &str) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    let mut pages = 0usize;
    loop {
        pages += 1;
        assert!(
            pages <= LISTING_PAGES,
            "case {case:?}: the listing of {bucket:?} must end inside {LISTING_PAGES} pages \
             of {PAGE_KEYS}, read {keys:?} and was handed another token"
        );
        let page = format!("/{bucket}?list-type=2&max-keys={PAGE_KEYS}");
        let path = match &token {
            Some(token) => format!("{page}&continuation-token={}", form_encode(token)),
            None => page,
        };
        let (status, body) = s3_text(router, "GET", &path, &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the bucket {bucket:?} must be listable, got {status} with {body:?}"
        );
        keys.extend(elements(&body, "Key"));
        match opt_element(&body, "NextContinuationToken") {
            Some(next) if element(&body, "IsTruncated") == "true" => token = Some(next),
            _ => break,
        }
    }
    keys.sort();
    keys
}

/// `GET /<bucket>/<key>` — `GetObject` — as the bytes it holds, or `None` when the store
/// does not hold it.
async fn object_bytes(
    router: &MultiServiceRouter,
    bucket: &str,
    key: &str,
) -> Option<Vec<u8>> {
    let (status, body) = s3(router, "GET", &format!("/{bucket}/{key}"), &[]).await;
    (status == 200).then_some(body)
}

/// `DELETE /<bucket>/<key>` — `DeleteObject`.
async fn delete_object(router: &MultiServiceRouter, case: &str, bucket: &str, key: &str) {
    let (status, body) = s3_text(router, "DELETE", &format!("/{bucket}/{key}"), &[]).await;
    assert_eq!(
        status, 204,
        "case {case:?}: the delete of {key:?} must be accepted, got {status} with {body:?}"
    );
}

/// `DELETE /<bucket>` — `DeleteBucket`: the status and the body it answered.
async fn delete_bucket(router: &MultiServiceRouter, bucket: &str) -> (u16, String) {
    s3_text(router, "DELETE", &format!("/{bucket}"), &[]).await
}

/// `HEAD /<bucket>` — `HeadBucket`, which is how a client asks whether the bucket is still
/// there.
async fn head_bucket(router: &MultiServiceRouter, bucket: &str) -> u16 {
    s3_text(router, "HEAD", &format!("/{bucket}"), &[]).await.0
}

/// A model card content document whose `model_description` is `description`.
fn card_content(description: &str) -> String {
    json!({
        "model_card_schema_version": "0.0.1",
        "model_overview": { "model_description": description },
    })
    .to_string()
}

/// Create `card` holding `versions` versions through the wire: the create mints version 1
/// and every `UpdateModelCard` appends the next.
async fn given_card(router: &MultiServiceRouter, case: &str, card: &str, versions: i32) {
    sagemaker_ok(
        router,
        case,
        "CreateModelCard",
        &json!({
            "ModelCardName": card,
            "ModelCardStatus": "Draft",
            "Content": card_content("version 1"),
        }),
    )
    .await;
    for version in 2..=versions {
        sagemaker_ok(
            router,
            case,
            "UpdateModelCard",
            &json!({
                "ModelCardName": card,
                "Content": card_content(&format!("version {version}")),
            }),
        )
        .await;
    }
}

/// Run one export job of `card` into `output_path`, wait for SageMaker to report it
/// terminal, and answer the Amazon S3 URI it reports for the document it wrote — read off
/// `ExportArtifacts.S3ExportArtifacts`, which is where the job reports the object it saved.
async fn exported_artifact(
    router: &MultiServiceRouter,
    case: &str,
    export: &Export,
    output_path: &str,
) -> String {
    let mut body = json!({
        "ModelCardName": export.card,
        "ModelCardExportJobName": export.job,
        "OutputConfig": { "S3OutputPath": output_path },
    });
    if let Some(version) = export.pinned {
        body["ModelCardVersion"] = json!(version);
    }
    let created = sagemaker_ok(router, case, "CreateModelCardExportJob", &body).await;
    let arn = created["ModelCardExportJobArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a job ARN in {created:?}"))
        .to_string();

    let began = Instant::now();
    loop {
        let reported = sagemaker_ok(
            router,
            case,
            "DescribeModelCardExportJob",
            &json!({ "ModelCardExportJobArn": &arn }),
        )
        .await;
        let status = reported["Status"].as_str().unwrap_or_default().to_string();
        if status != "InProgress" {
            assert_eq!(
                status, "Completed",
                "case {case:?}: the export must write its document, got {reported:?}"
            );
            return reported["ExportArtifacts"]["S3ExportArtifacts"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected the saved document in {reported:?}")
                })
                .to_string();
        }
        assert!(
            began.elapsed() < PATIENCE,
            "case {case:?}: SageMaker must settle the export inside {PATIENCE:?}, last read \
             {reported:?}"
        );
        ::tokio::time::sleep(LOOK).await;
    }
}

/// One export a row runs: which card (and how many versions that card holds), which version
/// the request pins, and the job's own name.
struct Export {
    card: &'static str,
    versions: i32,
    pinned: Option<i32>,
    job: &'static str,
}

/// How the row's client empties its bucket.
#[derive(Debug, Clone, Copy)]
enum Purge {
    /// One `DeleteObject` per artifact URI the service REPORTED, plus the client's own
    /// keys: the teardown of a client that only ever knew what it was handed.
    ReportedUris,
    /// One `DeleteObject` per key the listing reported.
    ListedKeys,
}

/// One row: a bucket, the output path the exports are submitted with, what gets written
/// into it, and how the client empties it.
struct Row {
    case: &'static str,
    bucket: &'static str,
    /// What the request's `S3OutputPath` names after the bucket; the empty string is the
    /// bucket root.
    output: &'static str,
    /// The key prefix that output path names: `output` carrying exactly one trailing
    /// delimiter, and none at all for the bucket root.
    key_prefix: &'static str,
    exports: &'static [Export],
    /// The keys the CLIENT writes itself, beside the documents the exports save.
    client_keys: &'static [&'static str],
    purge: Purge,
}

/// A bucket a model card export wrote into holds its document under exactly the key the job
/// reported, and emptying it through the client's own deletes makes `DeleteBucket` answer
/// 204 — while it still holds a document the delete is refused `BucketNotEmpty` (409).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_emptied_model_card_export_bucket_is_deleted() {
    let router = emulator();

    let rows = [
        Row {
            case: "one export under a nested prefix, emptied through the URI it reported",
            bucket: "sagemaker-card-export-nested",
            output: "model-card/export/",
            key_prefix: "model-card/export/",
            exports: &[Export {
                card: "nested-card",
                versions: 1,
                pinned: None,
                job: "nested-export",
            }],
            client_keys: &[],
            purge: Purge::ReportedUris,
        },
        Row {
            case: "two jobs of one card under a prefix written with no delimiter",
            bucket: "sagemaker-card-export-two-jobs",
            output: "exports",
            key_prefix: "exports/",
            exports: &[
                Export {
                    card: "twice-exported-card",
                    versions: 3,
                    pinned: Some(2),
                    job: "first-export",
                },
                Export {
                    card: "twice-exported-card",
                    versions: 3,
                    pinned: None,
                    job: "second-export",
                },
            ],
            client_keys: &[],
            purge: Purge::ReportedUris,
        },
        Row {
            case: "exports of two cards at the bucket root beside the client's own objects",
            bucket: "sagemaker-card-export-root",
            output: "",
            key_prefix: "",
            exports: &[
                Export {
                    card: "first-card",
                    versions: 1,
                    pinned: Some(1),
                    job: "root-export",
                },
                Export {
                    card: "second-card",
                    versions: 2,
                    pinned: None,
                    job: "root-export",
                },
            ],
            client_keys: &["governance/notes.json", "governance/owners.csv"],
            purge: Purge::ListedKeys,
        },
        Row {
            case: "one export under a deep prefix, emptied through the listing",
            bucket: "sagemaker-card-export-deep",
            output: "deeply/nested/prefix/",
            key_prefix: "deeply/nested/prefix/",
            exports: &[Export {
                card: "deep-card",
                versions: 4,
                pinned: Some(3),
                job: "deep-export",
            }],
            client_keys: &["deeply/nested/prefix/client-own.txt"],
            purge: Purge::ListedKeys,
        },
    ];

    for row in &rows {
        let case = row.case;
        let bucket = row.bucket;
        given_bucket(&router, case, bucket).await;

        let mut created_cards: Vec<&str> = Vec::new();
        for export in row.exports {
            if !created_cards.contains(&export.card) {
                given_card(&router, case, export.card, export.versions).await;
                created_cards.push(export.card);
            }
        }

        // What the SERVER wrote, and the key it HANDED the client for it: the artifact URI
        // is the one the request's own values compose — the output path it named, the card
        // as its own segment and the job as the file stem — so a client holding nothing but
        // what it asked for addresses the very object the export wrote.
        let output_path = format!("s3://{bucket}/{}", row.output);
        let mut reported: Vec<String> = Vec::new();
        for export in row.exports {
            let artifact = exported_artifact(&router, case, export, &output_path).await;
            let expected = format!(
                "s3://{bucket}/{}{}/{}.pdf",
                row.key_prefix, export.card, export.job
            );
            assert_eq!(
                artifact, expected,
                "case {case:?}: expected the artifact of {} / {} at {expected}, got \
                 {artifact}",
                export.card, export.job
            );
            reported.push(artifact);
        }

        // And what the CLIENT wrote itself.
        for key in row.client_keys {
            put_object(&router, case, bucket, key).await;
        }

        let under = format!("s3://{bucket}/");
        let reported_keys: Vec<String> = reported
            .iter()
            .map(|uri| {
                uri.strip_prefix(&under)
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: expected the artifact under {under}, got {uri}")
                    })
                    .to_string()
            })
            .collect();

        // The document really is at the key it was reported at, as the PDF the export
        // renders: a client that follows the URI it was handed reads it.
        for key in &reported_keys {
            let document = object_bytes(&router, bucket, key)
                .await
                .unwrap_or_else(|| panic!("case {case:?}: Amazon S3 must hold {key}"));
            assert!(
                document.starts_with(PDF_HEADER),
                "case {case:?}: {key} must be a PDF document, got {} byte(s) starting {:?}",
                document.len(),
                String::from_utf8_lossy(&document[..document.len().min(8)])
            );
        }

        // Nothing is held under a key no answer named: the bucket holds exactly the
        // documents the jobs reported and the objects the client wrote itself.
        let mut expected_keys: Vec<String> = reported_keys.clone();
        expected_keys.extend(row.client_keys.iter().map(|key| (*key).to_string()));
        expected_keys.sort();
        let listed = listed_keys(&router, case, bucket).await;
        assert_eq!(
            listed, expected_keys,
            "case {case:?}: the bucket must hold exactly the reported documents and the \
             client's own objects, got {listed:?}"
        );

        // While it holds them, the delete is refused and the bucket is left as it was.
        let (status, body) = delete_bucket(&router, bucket).await;
        assert_eq!(
            status, 409,
            "case {case:?}: a bucket holding {} object(s) must refuse the delete with 409, \
             got {status} with {body:?}",
            listed.len()
        );
        assert_eq!(
            opt_element(&body, "Code").as_deref(),
            Some(BUCKET_NOT_EMPTY),
            "case {case:?}: expected the refusal {BUCKET_NOT_EMPTY}, got {body:?}"
        );
        assert_eq!(
            head_bucket(&router, bucket).await,
            200,
            "case {case:?}: the refused delete must leave the bucket in place"
        );
        assert_eq!(
            listed_keys(&router, case, bucket).await,
            expected_keys,
            "case {case:?}: the refused delete must leave every object in place"
        );

        // The teardown, the way the row's client does it.
        match row.purge {
            Purge::ReportedUris => {
                for key in &reported_keys {
                    delete_object(&router, case, bucket, key).await;
                }
                for key in row.client_keys {
                    delete_object(&router, case, bucket, key).await;
                }
            }
            Purge::ListedKeys => {
                for key in &listed {
                    delete_object(&router, case, bucket, key).await;
                }
            }
        }

        let emptied = listed_keys(&router, case, bucket).await;
        assert!(
            emptied.is_empty(),
            "case {case:?}: the teardown must leave the bucket empty, got {emptied:?}"
        );

        // The delete a client makes next: 204, with no body.
        let (status, body) = delete_bucket(&router, bucket).await;
        assert_eq!(
            status, 204,
            "case {case:?}: an emptied bucket must be deleted, got {status} with {body:?}"
        );
        assert!(
            body.is_empty(),
            "case {case:?}: the delete answers no body, got {body:?}"
        );

        // And the bucket really stopped existing: the next read and the next delete both
        // answer the not-found of a name no bucket is held under.
        assert_eq!(
            head_bucket(&router, bucket).await,
            404,
            "case {case:?}: the deleted bucket must be gone"
        );
        let (status, body) = delete_bucket(&router, bucket).await;
        assert_eq!(
            status, 404,
            "case {case:?}: a second delete must answer not-found, got {status} with {body:?}"
        );
        assert_eq!(
            opt_element(&body, "Code").as_deref(),
            Some(NO_SUCH_BUCKET),
            "case {case:?}: expected {NO_SUCH_BUCKET}, got {body:?}"
        );
    }
}
