// SPDX-License-Identifier: CC0-1.0

//! Tearing down the results bucket of an Athena run: the objects a query SAVED are part of
//! the same object space a client lists and deletes, so emptying the bucket through those
//! calls makes `DeleteBucket` succeed.
//!
//! RULE: a bucket's emptiness for `DeleteBucket` is over the object versions the object
//! store holds for it — "All objects (including all object versions and delete markers) in
//! the bucket must be deleted before the bucket itself can be deleted" (`DeleteBucket`'s own
//! documentation) — and WHO wrote an object is not part of that question: an object another
//! emulated service saved into a bucket is stored, listed and deleted through the very same
//! object path a client's own write uses. So a client that removes everything a listing
//! reports has emptied the bucket, and the delete that follows answers 204 and the bucket
//! stops existing; while any version is still held the delete is refused with
//! `BucketNotEmpty` (409) and nothing about the bucket changes.
//!
//! A query execution's results are such an object: Athena "stores query results and metadata
//! information for each query that runs in a query result location that you can specify", and
//! `GetQueryExecution` reports the object it wrote in
//! `ResultConfiguration.OutputLocation` — so the universal teardown (list, delete everything
//! listed, delete the bucket) has to converge on a bucket an Athena run has written into.
//! Two crates have to agree for that to hold (Athena saves the results, Amazon S3 holds,
//! lists and deletes them), so the suite lives here rather than in either of them.
//!
//! The rows differ along what the rule generalises over: WHERE the results land (a nested
//! prefix, the bucket root), HOW MANY objects the bucket holds (one run's, two runs', and
//! runs beside the client's own table objects), HOW the client empties it (one `DeleteObject`
//! per key, one `DeleteObjects` batch, and a version-by-version purge), and whether the bucket
//! carries a versioning state at all — the case where a delete that names no version takes
//! nothing away, so the bucket is still not empty.
//!
//! Nothing here asserts a value this emulator invented: the result object's key is read off
//! Athena's own report of the query, the keys the teardown deletes are the ones the listing
//! reported, and the refusal's code is read out of the answer.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{blocks_of, element, elements, form_encode, opt_element};

/// The Region every call here is signed for and addressed in.
const REGION: &str = "us-west-2";

/// The XML namespace a client's Amazon S3 payload carries.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// How long a row waits for Athena to report a terminal query, and how often it looks while
/// waiting. The budget is generous next to the span a submission's completion falls due after
/// (`QUERY_EXECUTION_COMPLETION`), so a loaded machine has room.
const PATIENCE: Duration = Duration::from_secs(120);
const LOOK: Duration = Duration::from_millis(50);

/// How many keys one page of the teardown's listing asks for: small enough that a row holding
/// more than a couple of objects really pages, which is what makes the continuation token the
/// teardown follows part of what is exercised.
const PAGE_KEYS: usize = 2;

/// How many pages of a listing the teardown reads before the listing is not ending: a page
/// holds [`PAGE_KEYS`] keys and no row writes more than a handful of objects, so this is
/// generous, and it is what makes a listing that keeps handing out the same token a report
/// rather than a suite that never returns.
const LISTING_PAGES: usize = 20;

/// How many rounds of "list what is left, delete every version it names" the purge of a
/// versioned bucket may take before it is not converging: a round deletes every version the
/// listing reported, so one round per page of versions is already generous for the handful of
/// objects a row holds.
const PURGE_ROUNDS: usize = 20;

/// The statement every run here executes: a `SELECT` needs no table, and Athena classifies it
/// as DML, so the run saves a results object and the metadata object beside it.
const QUERY: &str = "SELECT 'north' AS region, 42 AS orders";

/// The suffix of the metadata object Athena saves beside a results object, read off the
/// service's own constant rather than spelled again here.
const RESULT_METADATA_SUFFIX: &str =
    service_com_amazonaws_athena_amazonathena::state::query_results::RESULT_METADATA_SUFFIX;

/// The code Amazon S3 refuses the delete of a bucket that still holds something with, and the
/// code it answers a request naming a bucket it does not hold with. Written out, not read off
/// the service's own constants: both are fixed by the Amazon S3 API, so a literal here is what
/// catches a wrong wire code — a constant would move with the handler and assert nothing.
const BUCKET_NOT_EMPTY: &str = "BucketNotEmpty";
const NO_SUCH_BUCKET: &str = "NoSuchBucket";

/// Amazon S3 and Athena over ONE shared engine, as `core/router/src/lib.rs` mounts them — so
/// the results Athena saves land in this very object store.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_athena_amazonathena::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header scoped to `service` in [`REGION`], which is what routes the
/// request and what a service reads the caller's Region from.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=abc"
    )
}

/// One raw request to the emulator's Amazon S3 front door: path-style, so the bucket and the
/// key live in the URI and the body is the payload.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &[u8]) -> (u16, String) {
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
    (
        status,
        String::from_utf8_lossy(response.into_body().as_ref()).to_string(),
    )
}

/// One `awsJson1_1` call to Athena, under the target prefix its model's `service_info` names.
async fn athena(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("athena.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AmazonAthena.{operation}"))
        .header("authorization", authorization("athena"))
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

/// `PUT /<bucket>` with the location constraint a bucket outside `us-east-1` carries.
async fn given_bucket(router: &MultiServiceRouter, case: &str, bucket: &str) {
    let configuration = format!(
        "<CreateBucketConfiguration xmlns=\"{S3_XMLNS}\">\
         <LocationConstraint>{REGION}</LocationConstraint></CreateBucketConfiguration>"
    );
    let (status, body) = s3(
        router,
        "PUT",
        &format!("/{bucket}"),
        configuration.as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the bucket {bucket:?} must be created, got {status} with {body:?}"
    );
}

/// `PUT /<bucket>?versioning` with a `BucketVersioningStatus`.
async fn set_versioning(router: &MultiServiceRouter, case: &str, bucket: &str, status: &str) {
    let payload = format!(
        "<VersioningConfiguration xmlns=\"{S3_XMLNS}\"><Status>{status}</Status>\
         </VersioningConfiguration>"
    );
    let (answered, body) = s3(
        router,
        "PUT",
        &format!("/{bucket}?versioning"),
        payload.as_bytes(),
    )
    .await;
    assert_eq!(
        answered, 200,
        "case {case:?}: the bucket {bucket:?} must take versioning {status:?}, got {answered} \
         with {body:?}"
    );
}

/// `PUT /<bucket>/<key>` — the client's own object write.
async fn put_object(router: &MultiServiceRouter, case: &str, bucket: &str, key: &str) {
    let payload = format!("the bytes of {key}");
    let (status, body) = s3(
        router,
        "PUT",
        &format!("/{bucket}/{key}"),
        payload.as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the write of {key:?} must be accepted, got {status} with {body:?}"
    );
}

/// Every key `bucket` holds, read through `ListObjectsV2` the way a teardown reads it:
/// following the listing's own continuation token until the last page.
///
/// Each request asks for [`PAGE_KEYS`] keys, so the token is really followed rather than
/// nominally: a row holding more than that answers a truncated page and the teardown reads the
/// rest through the token the page carried. The token is a server value a client sends back
/// verbatim, so it is percent-encoded into the query the way any other component is.
async fn listed_keys(router: &MultiServiceRouter, case: &str, bucket: &str) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    let mut pages = 0usize;
    loop {
        pages += 1;
        assert!(
            pages <= LISTING_PAGES,
            "case {case:?}: the listing of {bucket:?} must end inside {LISTING_PAGES} pages of \
             {PAGE_KEYS}, read {keys:?} and was handed another token"
        );
        let page = format!("/{bucket}?list-type=2&max-keys={PAGE_KEYS}");
        let path = match &token {
            Some(token) => format!("{page}&continuation-token={}", form_encode(token)),
            None => page,
        };
        let (status, body) = s3(router, "GET", &path, &[]).await;
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

/// Every `(key, version id)` the bucket still holds, read through `ListObjectVersions` — the
/// call a teardown of a versioned bucket reads, because it is the only one that reports the
/// versions a delete marker hides.
///
/// Paged like [`listed_keys`], through this listing's OWN resume markers: each request asks for
/// [`PAGE_KEYS`] keys and the next one carries the `NextKeyMarker` / `NextVersionIdMarker` the
/// truncated page handed back, so what the purge is handed is every version rather than the
/// first page of them.
async fn listed_versions(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
) -> Vec<(String, String)> {
    let mut entries: Vec<(String, String)> = Vec::new();
    let mut markers: Option<(String, String)> = None;
    let mut pages = 0usize;
    loop {
        pages += 1;
        assert!(
            pages <= LISTING_PAGES,
            "case {case:?}: the version listing of {bucket:?} must end inside {LISTING_PAGES} \
             pages of {PAGE_KEYS}, read {entries:?} and was handed another marker"
        );
        let page = format!("/{bucket}?versions&max-keys={PAGE_KEYS}");
        let path = match &markers {
            Some((key, version)) => format!(
                "{page}&key-marker={}&version-id-marker={}",
                form_encode(key),
                form_encode(version)
            ),
            None => page,
        };
        let (status, body) = s3(router, "GET", &path, &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a version listing of {bucket:?} must succeed, got {status} with \
             {body:?}"
        );
        // Each entry arrives in a `Version` or a `DeleteMarker` container, and both carry the
        // key and the version id that names it — read per container, so each pair comes off the
        // entry that carries both.
        entries.extend(
            ["Version", "DeleteMarker"]
                .into_iter()
                .flat_map(|container| blocks_of(&body, container))
                .filter_map(|entry| {
                    Some((
                        opt_element(entry, "Key")?,
                        opt_element(entry, "VersionId")?,
                    ))
                }),
        );
        if opt_element(&body, "IsTruncated").as_deref() != Some("true") {
            return entries;
        }
        match (
            opt_element(&body, "NextKeyMarker"),
            opt_element(&body, "NextVersionIdMarker"),
        ) {
            (Some(key), Some(version)) => markers = Some((key, version)),
            // A truncated page that names no resume marker is the end of what this reader can
            // follow; the purge re-lists after each round, so it reports what is left either
            // way.
            _ => return entries,
        }
    }
}

/// `DELETE /<bucket>/<key><query>` — `DeleteObject`, with whatever query the caller names.
async fn delete_object(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    key: &str,
    query: &str,
) {
    let (status, body) = s3(
        router,
        "DELETE",
        &format!("/{bucket}/{key}{query}"),
        &[],
    )
    .await;
    assert_eq!(
        status, 204,
        "case {case:?}: the delete of {key:?}{query} must be accepted, got {status} with {body:?}"
    );
}

/// `POST /<bucket>?delete` — `DeleteObjects` over every key the listing reported, which is the
/// batch a teardown sends.
async fn delete_objects(router: &MultiServiceRouter, case: &str, bucket: &str, keys: &[String]) {
    let mut payload = format!("<Delete xmlns=\"{S3_XMLNS}\">");
    for key in keys {
        payload.push_str(&format!("<Object><Key>{key}</Key></Object>"));
    }
    payload.push_str("</Delete>");
    let (status, body) = s3(
        router,
        "POST",
        &format!("/{bucket}?delete"),
        payload.as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the batch delete must be accepted, got {status} with {body:?}"
    );
    assert!(
        !body.contains("<Error>"),
        "case {case:?}: no key of the batch may be reported as an error, got {body:?}"
    );
    let mut deleted = elements(&body, "Key");
    deleted.sort();
    let mut asked: Vec<String> = keys.to_vec();
    asked.sort();
    assert_eq!(
        deleted, asked,
        "case {case:?}: the batch must report every key it deleted, got {body:?}"
    );
}

/// `DELETE /<bucket>` — `DeleteBucket`: the status and the body it answered.
async fn delete_bucket(router: &MultiServiceRouter, bucket: &str) -> (u16, String) {
    s3(router, "DELETE", &format!("/{bucket}"), &[]).await
}

/// `HEAD /<bucket>` — `HeadBucket`, which is how a client asks whether the bucket is still
/// there.
async fn head_bucket(router: &MultiServiceRouter, bucket: &str) -> u16 {
    s3(router, "HEAD", &format!("/{bucket}"), &[]).await.0
}

/// Submit `QUERY` with its results going to `location`, wait for Athena to report the query
/// terminal, and answer the KEY of the results object its run saved — read off
/// `ResultConfiguration.OutputLocation`, which is where Athena reports the object it wrote.
async fn ran_query(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    location: &str,
) -> String {
    let (status, submitted) = athena(
        router,
        "StartQueryExecution",
        &json!({
            "QueryString": QUERY,
            "ResultConfiguration": { "OutputLocation": location },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the query must be submitted, got {status} with {submitted:?}"
    );
    let query_id = submitted["QueryExecutionId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueryExecutionId in {submitted:?}"))
        .to_string();

    let began = Instant::now();
    loop {
        let (status, reported) = athena(
            router,
            "GetQueryExecution",
            &json!({ "QueryExecutionId": &query_id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the query must stay readable, got {status} with {reported:?}"
        );
        let state = reported["QueryExecution"]["Status"]["State"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if !matches!(state.as_str(), "QUEUED" | "RUNNING") {
            assert_eq!(
                state, "SUCCEEDED",
                "case {case:?}: the query must run, got {reported:?}"
            );
            let output = reported["QueryExecution"]["ResultConfiguration"]["OutputLocation"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected the saved results object in {reported:?}")
                })
                .to_string();
            let under = format!("s3://{bucket}/");
            return output
                .strip_prefix(&under)
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: expected the results object under {under}, got {output:?}"
                    )
                })
                .to_string();
        }
        assert!(
            began.elapsed() < PATIENCE,
            "case {case:?}: Athena must settle the query inside {PATIENCE:?}, last read \
             {reported:?}"
        );
        ::tokio::time::sleep(LOOK).await;
    }
}

/// How the row's client empties its bucket.
#[derive(Debug, Clone, Copy)]
enum Purge {
    /// One `DeleteObject` per key the listing reported.
    OneByOne,
    /// One `DeleteObjects` batch over every key the listing reported.
    Batch,
    /// A delete that names NO version first — which, on a versioned bucket, takes nothing
    /// away — and then one `DeleteObject` per version the version listing reports.
    ByVersion,
}

/// One row: a bucket, what gets written into it, and how the client empties it.
struct Row {
    case: &'static str,
    bucket: &'static str,
    /// The `s3://` results location each of the row's queries is submitted with, relative to
    /// the bucket: the empty string is the bucket root.
    prefix: &'static str,
    /// How many queries run, each saving its own results objects.
    queries: usize,
    /// The keys the CLIENT writes itself, beside what the queries save.
    client_keys: &'static [&'static str],
    /// The `BucketVersioningStatus` the bucket carries, where the row sets one.
    versioning: Option<&'static str>,
    purge: Purge,
}

/// A bucket emptied through the client's own listing and deletes — whatever wrote the objects
/// it held — is deleted with 204, and is refused with `BucketNotEmpty` (409) for exactly as
/// long as it still holds a version.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_emptied_results_bucket_is_deleted() {
    let router = emulator();

    let rows = vec![
        Row {
            case: "one run's results under a nested prefix, emptied key by key",
            bucket: "athena-teardown-nested",
            prefix: "query-results/",
            queries: 1,
            client_keys: &[],
            versioning: None,
            purge: Purge::OneByOne,
        },
        Row {
            case: "results at the bucket root beside the client's own table objects",
            bucket: "athena-teardown-root",
            prefix: "",
            queries: 1,
            client_keys: &[
                "table/metadata/00000-snapshot.metadata.json",
                "table/data/part-00000.parquet",
            ],
            versioning: None,
            purge: Purge::Batch,
        },
        Row {
            case: "two runs' results under one prefix, emptied in one batch",
            bucket: "athena-teardown-two-runs",
            prefix: "iceberg/results/",
            queries: 2,
            client_keys: &[],
            versioning: None,
            purge: Purge::Batch,
        },
        Row {
            case: "a versioning-enabled bucket, purged version by version",
            bucket: "athena-teardown-versioned",
            prefix: "versioned-results/",
            queries: 1,
            client_keys: &["table/data/part-00000.parquet"],
            versioning: Some("Enabled"),
            purge: Purge::ByVersion,
        },
    ];

    for row in &rows {
        let case = row.case;
        let bucket = row.bucket;
        given_bucket(&router, case, bucket).await;
        if let Some(status) = row.versioning {
            set_versioning(&router, case, bucket, status).await;
        }

        // What the SERVER wrote: each run's results object, named by Athena's own report, and
        // the metadata object it saves beside it.
        let location = format!("s3://{bucket}/{}", row.prefix);
        let mut expected: Vec<String> = Vec::new();
        for _ in 0..row.queries {
            let key = ran_query(&router, case, bucket, &location).await;
            expected.push(format!("{key}{RESULT_METADATA_SUFFIX}"));
            expected.push(key);
        }
        // And what the CLIENT wrote itself.
        for key in row.client_keys {
            put_object(&router, case, bucket, key).await;
            expected.push(key.to_string());
        }
        expected.sort();

        // The saved results are in the SAME object space the client lists: a teardown that
        // reads the bucket finds every one of them and nothing else.
        let listed = listed_keys(&router, case, bucket).await;
        assert_eq!(
            listed, expected,
            "case {case:?}: the listing must report every object the bucket holds, got {listed:?}"
        );

        // While it holds them, the delete is refused and the bucket is left exactly as it was.
        let (status, body) = delete_bucket(&router, bucket).await;
        assert_eq!(
            status, 409,
            "case {case:?}: a bucket holding {} objects must refuse the delete with 409, got \
             {status} with {body:?}",
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
            expected,
            "case {case:?}: the refused delete must leave every object in place"
        );

        // The teardown: everything the listing reported, removed the way the row's client
        // removes it.
        match row.purge {
            Purge::OneByOne => {
                for key in &listed {
                    delete_object(&router, case, bucket, key, "").await;
                }
            }
            Purge::Batch => delete_objects(&router, case, bucket, &listed).await,
            Purge::ByVersion => {
                // A delete that names no version lays a delete marker over the version it
                // displaces: the key leaves the current view, and the bucket still holds both.
                for key in &listed {
                    delete_object(&router, case, bucket, key, "").await;
                }
                let current = listed_keys(&router, case, bucket).await;
                assert!(
                    current.is_empty(),
                    "case {case:?}: a delete marker hides the key from the current view, got \
                     {current:?}"
                );
                let (status, body) = delete_bucket(&router, bucket).await;
                assert_eq!(
                    status, 409,
                    "case {case:?}: the versions behind the delete markers are still held, so \
                     the delete must be refused, got {status} with {body:?}"
                );
                assert_eq!(
                    opt_element(&body, "Code").as_deref(),
                    Some(BUCKET_NOT_EMPTY),
                    "case {case:?}: expected the refusal {BUCKET_NOT_EMPTY}, got {body:?}"
                );
                // Every version the listing reports, round after round: the listing is asked
                // again after each one, so the purge is over what is LEFT rather than over a
                // single page of it, and it has to converge inside a bound no correct store
                // reaches.
                let mut rounds = 0usize;
                loop {
                    let held = listed_versions(&router, case, bucket).await;
                    if held.is_empty() {
                        break;
                    }
                    rounds += 1;
                    assert!(
                        rounds <= PURGE_ROUNDS,
                        "case {case:?}: the purge must converge inside {PURGE_ROUNDS} rounds, \
                         still held {held:?}"
                    );
                    for (key, version) in held {
                        delete_object(
                            &router,
                            case,
                            bucket,
                            &key,
                            &format!("?versionId={version}"),
                        )
                        .await;
                    }
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

        // And the bucket really stopped existing, rather than being reported as deleted: the
        // next read and the next delete both answer the not-found of a name no bucket is held
        // under.
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
