// SPDX-License-Identifier: CC0-1.0

//! What an AWS Data Exchange export leaves behind in Amazon S3, across two services on one
//! engine: Data Exchange holds the data set, the revision, its assets and the jobs, and
//! Amazon S3 holds the destination bucket and answers the read of the object the export
//! wrote.
//!
//! RULE: an export job that reaches `COMPLETED` has WRITTEN one Amazon S3 object per exported
//! asset, at the bucket and key its own `Details` name, so that key names a stored object like
//! any other the bucket holds: `GetObject` of it answers 200 with the asset's bytes as the
//! body — never the 404 of a key nothing wrote — and reports the stored object's own metadata
//! in the headers the model binds `GetObjectOutput` to (`smithy_query(query="operation",
//! operation="GetObject")`): `AcceptRanges` -> `accept-ranges`, `ContentType` ->
//! `Content-Type`, `ContentLength` -> `Content-Length`, `ETag` -> `ETag`, `LastModified` ->
//! `Last-Modified`, `ServerSideEncryption` -> `x-amz-server-side-encryption`, and
//! `ChecksumType` -> `x-amz-checksum-type` beside the `Checksum*` header of the algorithm the
//! object was stored under — the checksum only where the request enables checksum mode ("to
//! retrieve the checksum, this mode must be enabled", `GetObjectRequest$ChecksumMode`). Each
//! of those values describes THOSE bytes: the media type the asset carries, the byte count it
//! holds, and an entity tag and a checksum equal to the ones a client's own write of the same
//! bytes is stored with.
//!
//! Only a cross-service suite can pin this: the data set, the assets and the jobs are Data
//! Exchange's, and the bucket, the object and its metadata are Amazon S3's, so the rule is
//! observable only with both mounted on one engine. The Data Exchange crate's own export suite
//! reads the body and the media type of an exported object; the metadata a read reports for it
//! — the entity tag, the modification instant, the checksum pair and the encryption — is
//! asserted here.
//!
//! The rows vary what the rule generalises over. One table is the ASSETS, differing along the
//! media type the source object declares (a text type, a structured type, a binary type and
//! one carrying a parameter), the bytes it holds (ASCII text, JSON, bytes that are not UTF-8,
//! and no bytes at all) and their length. The other is the DESTINATIONS, differing along both
//! export job types, the key form each destination names (a nested prefix, a bare name, no key
//! at all so the asset's own name is used, and a revision key pattern) and whether the job
//! asks for an encryption of its own. Every asset is read back out of every destination, so
//! each pair is one case.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for.
const REGION: &str = "us-west-2";

/// The long-term key every emulator publishes, which is what these calls are signed with. The
/// signature itself is never checked: this router runs with request authentication off, as
/// every suite in this crate runs it.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The `AssetType` of a data set whose revisions hold Amazon S3 objects, from the model's
/// `AssetType` enum (`smithy_query(query="shape", shape="AssetType")`).
const SNAPSHOT_ASSET_TYPE: &str = "S3_SNAPSHOT";

/// The job `Type` values this suite drives, each with the `Details` member the model pairs it
/// with (`smithy_query(query="shape", shape="Type,RequestDetails")`).
const IMPORT_FROM_S3_TYPE: &str = "IMPORT_ASSETS_FROM_S3";
const EXPORT_ASSETS_TYPE: &str = "EXPORT_ASSETS_TO_S3";
const EXPORT_REVISIONS_TYPE: &str = "EXPORT_REVISIONS_TO_S3";

/// The terminal `State` values a job cannot leave on its own, which is what a client's
/// wait-for-the-job loop polls for. Source: `smithy_query(query="shape", shape="State")`
/// reports `[WAITING, IN_PROGRESS, ERROR, COMPLETED, CANCELLED, TIMED_OUT]`; the first two are
/// the ones a job still moves out of.
const TERMINAL_STATES: &[&str] = &["ERROR", "COMPLETED", "CANCELLED", "TIMED_OUT"];

/// The `State` a job that carried its work out rests in.
const COMPLETED: &str = "COMPLETED";

/// What a read answers in `accept-ranges` for an object it serves whole:
/// "`bytes`" is the one range unit Amazon S3 publishes (`GetObjectOutput$AcceptRanges`).
const ACCEPT_RANGES: &str = "bytes";

/// The `ServerSideEncryption` value of Amazon S3 managed encryption: "by default, all buckets
/// have a default encryption configuration that uses server-side encryption with Amazon S3
/// managed keys (SSE-S3)" (`GetBucketEncryption`), and SSE-S3 is `AES256`
/// (`smithy_query(query="shape", shape="ServerSideEncryption")`) — so every object a bucket
/// holds that named no encryption of its own is stored under it.
const SSE_S3: &str = "AES256";

/// The `ChecksumType` of an object one whole write stored: "for `PutObject` uploads, the
/// checksum type is always `FULL_OBJECT`" (`PutObjectRequest$ChecksumType`).
const FULL_OBJECT: &str = "FULL_OBJECT";

/// Each `ChecksumAlgorithm` value paired with the response header a read reports its digest
/// in.
///
/// Source: the model's `ChecksumAlgorithm` enum
/// (`smithy_query(query="shape", shape="ChecksumAlgorithm")` reports `[CRC32, CRC32C, SHA1,
/// SHA256, CRC64NVME, SHA512, MD5, XXHASH64, XXHASH3, XXHASH128]`) paired with
/// `GetObjectOutput`'s own `Checksum*` header bindings. Stated here so this suite does not
/// depend on WHICH algorithm an exporter's write picks: it reads the algorithm off the answer
/// and re-derives the digest under that same algorithm.
const CHECKSUM_HEADER_FOR_ALGORITHM: &[(&str, &str)] = &[
    ("CRC32", "x-amz-checksum-crc32"),
    ("CRC32C", "x-amz-checksum-crc32c"),
    ("SHA1", "x-amz-checksum-sha1"),
    ("SHA256", "x-amz-checksum-sha256"),
    ("CRC64NVME", "x-amz-checksum-crc64nvme"),
    ("SHA512", "x-amz-checksum-sha512"),
    ("MD5", "x-amz-checksum-md5"),
    ("XXHASH64", "x-amz-checksum-xxhash64"),
    ("XXHASH3", "x-amz-checksum-xxhash3"),
    ("XXHASH128", "x-amz-checksum-xxhash128"),
];

/// How long one poll of a clock-owed job transition waits before asking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long a poll loop waits for a job to reach a terminal state before giving up. Far past
/// the span this emulator owes one, so a loaded machine does not fail a case the clock would
/// have settled.
const POLL_BUDGET: Duration = Duration::from_secs(120);

/// The two services this rule spans, over ONE engine — what the assembled emulator does.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_dataexchange_dataexchange::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`: the scope is
/// how the router picks the service a request is for, and the Region the answers carry.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={AMBIENT_KEY}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// What one call answered: the status, the response headers by lower-case name, and the body.
struct Answer {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

impl Answer {
    /// One response header, where the answer carries it.
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }

    /// The body as text, for a failure message.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The body as JSON, `Null` when it is neither JSON nor present.
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// Dispatch one request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Answer {
    let mut request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = router
        .dispatch(
            request
                .body(Bytes::copy_from_slice(body))
                .expect("the test request is well formed"),
        )
        .await;
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_ascii_lowercase(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect();
    Answer {
        status,
        headers,
        body: response.body().to_vec(),
    }
}

/// One path-style Amazon S3 request, with the extra request headers `headers` on it.
async fn s3(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Answer {
    let credential = authorization("s3");
    let mut with_credential: Vec<(&str, &str)> = headers.to_vec();
    with_credential.push(("authorization", &credential));
    dispatch(
        router,
        method,
        &format!("s3.{REGION}.amazonaws.com"),
        path,
        &with_credential,
        body,
    )
    .await
}

/// One AWS Data Exchange call, as a restJson1 client makes it.
async fn dataexchange(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Answer {
    let credential = authorization("dataexchange");
    let mut headers: Vec<(&str, &str)> = vec![("authorization", &credential)];
    let payload = match body {
        Some(value) => {
            headers.push(("content-type", "application/json"));
            serde_json::to_vec(value).expect("a serializable request body")
        }
        None => Vec::new(),
    };
    dispatch(
        router,
        method,
        &format!("dataexchange.{REGION}.amazonaws.com"),
        path,
        &headers,
        &payload,
    )
    .await
}

/// One AWS Data Exchange call that has to be accepted, answering its body as JSON.
async fn dataexchange_ok(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Value {
    let answer = dataexchange(router, method, path, body).await;
    assert!(
        (200..300).contains(&answer.status),
        "case {case:?}: {method} {path} must be accepted, got {} with {:?}",
        answer.status,
        answer.text()
    );
    answer.json()
}

/// Create one bucket, naming the Region these requests are signed for as its location
/// constraint — "if you don't specify a Region, the bucket is created in the US East (N.
/// Virginia) Region (us-east-1) by default" (`CreateBucketConfiguration$LocationConstraint`),
/// which a regional endpoint refuses as incompatible with itself.
async fn create_bucket(router: &MultiServiceRouter, case: &str, bucket: &str) {
    let configuration = format!(
        "<CreateBucketConfiguration><LocationConstraint>{REGION}</LocationConstraint>\
         </CreateBucketConfiguration>"
    );
    let created = s3(
        router,
        "PUT",
        &format!("/{bucket}"),
        &[("content-type", "application/xml")],
        configuration.as_bytes(),
    )
    .await;
    assert!(
        (200..300).contains(&created.status),
        "case {case:?}: CreateBucket({bucket}) must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
}

/// Write one object through Amazon S3's own front door, with the extra request headers
/// `headers` on the write, which has to succeed.
async fn put_object(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    key: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) {
    let written = s3(router, "PUT", &format!("/{bucket}/{key}"), headers, body).await;
    assert!(
        (200..300).contains(&written.status),
        "case {case:?}: PutObject({bucket}/{key}) must succeed, got {} with {:?}",
        written.status,
        written.text()
    );
}

/// `GetObject` of one key, asking for the stored checksum as well — "to retrieve the checksum,
/// this mode must be enabled" (`GetObjectRequest$ChecksumMode`).
async fn get_object(router: &MultiServiceRouter, bucket: &str, key: &str) -> Answer {
    s3(
        router,
        "GET",
        &format!("/{bucket}/{key}"),
        &[("x-amz-checksum-mode", "ENABLED")],
        &[],
    )
    .await
}

/// `GetObject` of one key WITHOUT checksum mode, which is how a read that does not ask for the
/// checksum arrives.
async fn get_object_unchecked(router: &MultiServiceRouter, bucket: &str, key: &str) -> Answer {
    s3(router, "GET", &format!("/{bucket}/{key}"), &[], &[]).await
}

/// A data set holding one revision, and the two ids that name it.
async fn data_set_with_revision(router: &MultiServiceRouter, case: &str) -> (String, String) {
    let data_set = dataexchange_ok(
        router,
        case,
        "POST",
        "/v1/data-sets",
        Some(&json!({
            "AssetType": SNAPSHOT_ASSET_TYPE,
            "Description": "for an export read back out of Amazon S3",
            "Name": format!("export-round-trip-{case}"),
        })),
    )
    .await;
    let data_set_id = id_of(case, &data_set, "CreateDataSet");
    let revision = dataexchange_ok(
        router,
        case,
        "POST",
        &format!("/v1/data-sets/{data_set_id}/revisions"),
        Some(&json!({ "Comment": "for an export read back out of Amazon S3" })),
    )
    .await;
    let revision_id = id_of(case, &revision, "CreateRevision");
    (data_set_id, revision_id)
}

/// The `Id` one answer carries.
fn id_of(case: &str, answer: &Value, operation: &str) -> String {
    answer["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: {operation} must answer an Id, got {answer:?}"))
        .to_string()
}

/// Create a job of `job_type` over `details`, start it, and poll until the clock has settled it
/// — answering the job as `GetJob` then reports it.
async fn settled_job(
    router: &MultiServiceRouter,
    case: &str,
    job_type: &str,
    details: Value,
) -> Value {
    let created = dataexchange_ok(
        router,
        case,
        "POST",
        "/v1/jobs",
        Some(&json!({ "Type": job_type, "Details": details })),
    )
    .await;
    let job_id = id_of(case, &created, "CreateJob");
    dataexchange_ok(router, case, "PATCH", &format!("/v1/jobs/{job_id}"), None).await;

    // The execution a start sets going is owed its terminal state one span later, so the wait
    // is for the CONDITION — a state the job cannot leave — under a generous deadline.
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let job =
            dataexchange_ok(router, case, "GET", &format!("/v1/jobs/{job_id}"), None).await;
        let state = job["State"].as_str().unwrap_or_default().to_string();
        if TERMINAL_STATES.contains(&state.as_str()) {
            return job;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the {job_type} job never reached a terminal State, last {state:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One source object of the import, and so one asset of the revision the export writes out.
struct Asset {
    case: &'static str,
    /// The Amazon S3 key the source object is staged under, which is also the asset's `Name`:
    /// "when importing from Amazon S3, the Amazon S3 object key is used as the asset name"
    /// (`AssetEntry$Name`).
    key: &'static str,
    /// The media type the source object declares, which the exported copy of it declares too.
    media_type: &'static str,
    content: &'static [u8],
}

/// The assets table: what the rule generalises over on the CONTENT side — the media type a
/// source object declares (a text type, a structured type, a binary type, and one carrying a
/// charset parameter) and the bytes it holds (ASCII text with a trailing line break, JSON,
/// text without one, bytes that are not valid UTF-8, and no bytes at all).
fn assets() -> Vec<Asset> {
    vec![
        Asset {
            case: "a CSV asset",
            key: "source/orders.csv",
            media_type: "text/csv",
            content: b"order_id,sku,qty\n1001,A-17,3\n1002,B-42,1\n",
        },
        Asset {
            case: "a JSON asset",
            key: "source/report.json",
            media_type: "application/json",
            content: br#"{"rows":2,"complete":true}"#,
        },
        Asset {
            case: "a media type carrying a parameter",
            key: "source/notes.txt",
            media_type: "text/plain; charset=utf-8",
            content: b"first line\nsecond line without a trailing break",
        },
        Asset {
            case: "bytes that are not UTF-8",
            key: "source/pixels.bin",
            media_type: "application/octet-stream",
            content: &[0x00, 0xff, 0x10, 0x80, 0x7f, 0xfe, 0x00, 0x01],
        },
        Asset {
            case: "an asset holding no bytes at all",
            key: "source/empty.csv",
            media_type: "text/csv",
            content: b"",
        },
    ]
}

/// Which export a destination row drives, and the key form it names.
enum Destination {
    /// `EXPORT_ASSETS_TO_S3`, whose destination names one asset and MAY name the object key to
    /// write it under; `None` leaves the asset's own name to be the key ("when exporting to
    /// Amazon S3, the asset name is used as default target Amazon S3 object key",
    /// `AssetEntry$Name`).
    Assets {
        /// The key for the asset at each index of [`assets`], in that order.
        keys: &'static [Option<&'static str>],
    },
    /// `EXPORT_REVISIONS_TO_S3`, whose destination names the whole revision and MAY name the
    /// key pattern its assets are written under; `None` leaves each asset's own name.
    Revisions {
        key_pattern: Option<&'static str>,
    },
}

/// One destination row: one export job, into its own bucket.
struct DestinationRow {
    case: &'static str,
    destination: Destination,
    /// The `Encryption` the job asks for, where it asks for one at all.
    encryption: Option<&'static str>,
}

/// The destinations table: what the rule generalises over on the DESTINATION side — both
/// export job types, the key form a destination names (a nested prefix, a bare name with
/// another extension, a bare name with none, no key at all, and a revision key pattern over
/// two of the variables such a pattern admits), and whether the job asks for an encryption of
/// its own.
fn destination_rows() -> Vec<DestinationRow> {
    vec![
        DestinationRow {
            case: "asset destinations naming their own keys",
            destination: Destination::Assets {
                keys: &[
                    Some("export/nested/orders.csv"),
                    Some("report-copy.json"),
                    None,
                    Some("pixels"),
                    Some("export/empty.csv"),
                ],
            },
            encryption: None,
        },
        DestinationRow {
            case: "a revision destination naming a key pattern",
            destination: Destination::Revisions {
                key_pattern: Some("${Revision.Id}/${Asset.Name}"),
            },
            encryption: None,
        },
        DestinationRow {
            case: "a revision destination naming no key pattern, asked to encrypt",
            destination: Destination::Revisions { key_pattern: None },
            encryption: Some(SSE_S3),
        },
    ]
}

/// The key one asset of a row is expected at, from the key form that row names.
fn expected_key(row: &DestinationRow, index: usize, asset: &Asset, revision_id: &str) -> String {
    match &row.destination {
        Destination::Assets { keys } => keys
            .get(index)
            .copied()
            .flatten()
            .unwrap_or(asset.key)
            .to_string(),
        Destination::Revisions { key_pattern } => match key_pattern {
            // The variables a key pattern admits, as the documentation
            // `RevisionDestinationEntry$KeyPattern` links to states them.
            Some(pattern) => pattern
                .replace("${Revision.Id}", revision_id)
                .replace("${Asset.Name}", asset.key),
            None => asset.key.to_string(),
        },
    }
}

/// The `Details` of the export job one destination row drives.
fn export_details(
    row: &DestinationRow,
    bucket: &str,
    data_set_id: &str,
    revision_id: &str,
    asset_ids: &[String],
) -> (&'static str, Value) {
    match &row.destination {
        Destination::Assets { keys } => {
            let destinations: Vec<Value> = asset_ids
                .iter()
                .enumerate()
                .map(|(index, asset_id)| {
                    let mut entry = json!({ "AssetId": asset_id, "Bucket": bucket });
                    if let Some(key) = keys.get(index).copied().flatten() {
                        entry["Key"] = json!(key);
                    }
                    entry
                })
                .collect();
            let mut detail = json!({
                "AssetDestinations": destinations,
                "DataSetId": data_set_id,
                "RevisionId": revision_id,
            });
            with_encryption(&mut detail, row.encryption);
            (EXPORT_ASSETS_TYPE, json!({ "ExportAssetsToS3": detail }))
        }
        Destination::Revisions { key_pattern } => {
            let mut entry = json!({ "Bucket": bucket, "RevisionId": revision_id });
            if let Some(pattern) = key_pattern {
                entry["KeyPattern"] = json!(pattern);
            }
            let mut detail = json!({
                "DataSetId": data_set_id,
                "RevisionDestinations": [entry],
            });
            with_encryption(&mut detail, row.encryption);
            (
                EXPORT_REVISIONS_TYPE,
                json!({ "ExportRevisionsToS3": detail }),
            )
        }
    }
}

/// Put the `Encryption` a row asks for on its export details, where it asks for one.
fn with_encryption(detail: &mut Value, encryption: Option<&str>) {
    if let Some(encryption_type) = encryption {
        detail["Encryption"] = json!({ "Type": encryption_type });
    }
}

/// The `Errors` a settled job reports, for a failure message.
fn errors_of(job: &Value) -> String {
    format!("{:?}", job.get("Errors").unwrap_or(&Value::Null))
}

/// The `ChecksumAlgorithm` a read reports a digest under, and that digest — exactly one of the
/// `Checksum*` headers, since one whole write stores one checksum.
fn reported_checksum(case: &str, read: &Answer) -> (&'static str, String) {
    let reported: Vec<(&'static str, String)> = CHECKSUM_HEADER_FOR_ALGORITHM
        .iter()
        .filter_map(|(algorithm, header)| {
            read.header(header)
                .map(|digest| (*algorithm, digest.to_string()))
        })
        .collect();
    assert_eq!(
        reported.len(),
        1,
        "case {case:?}: a read with checksum mode enabled reports the one checksum the write \
         stored, got {reported:?} ({:?})",
        read.headers
    );
    reported.into_iter().next().expect("one reported checksum")
}

/// Every asset of [`assets`], imported into one revision out of a source bucket, and the asset
/// ids the import minted — in the order [`assets`] names them.
async fn imported_revision(
    router: &MultiServiceRouter,
    source_bucket: &str,
    data_set_id: &str,
    revision_id: &str,
) -> Vec<String> {
    for asset in assets() {
        put_object(
            router,
            asset.case,
            source_bucket,
            asset.key,
            &[("content-type", asset.media_type)],
            asset.content,
        )
        .await;
    }
    let sources: Vec<Value> = assets()
        .iter()
        .map(|asset| json!({ "Bucket": source_bucket, "Key": asset.key }))
        .collect();
    let imported = settled_job(
        router,
        "the import the exports read from",
        IMPORT_FROM_S3_TYPE,
        json!({ "ImportAssetsFromS3": {
            "AssetSources": sources,
            "DataSetId": data_set_id,
            "RevisionId": revision_id,
        }}),
    )
    .await;
    assert_eq!(
        imported["State"].as_str(),
        Some(COMPLETED),
        "the import every export reads from must COMPLETE, got {:?} with Errors {}",
        imported["State"],
        errors_of(&imported)
    );

    let listed = dataexchange_ok(
        router,
        "the import the exports read from",
        "GET",
        &format!("/v1/data-sets/{data_set_id}/revisions/{revision_id}/assets"),
        None,
    )
    .await;
    let entries = listed["Assets"].as_array().cloned().unwrap_or_default();
    assets()
        .iter()
        .map(|asset| {
            entries
                .iter()
                .find(|entry| entry["Name"].as_str() == Some(asset.key))
                .and_then(|entry| entry["Id"].as_str())
                .unwrap_or_else(|| {
                    panic!(
                        "case {:?}: the import must leave an asset named {:?}, got {entries:?}",
                        asset.case, asset.key
                    )
                })
                .to_string()
        })
        .collect()
}

/// THE RULE: every object a completed export wrote is a stored Amazon S3 object, so a read of
/// its key answers 200 with the asset's bytes and the stored object's own metadata in headers
/// — the media type the asset carries, the byte count, an entity tag and a checksum that
/// describe those bytes (the checksum only where the read enables checksum mode), the
/// modification instant, and the encryption the object is stored under.
#[tokio::test]
async fn an_exported_asset_is_readable_with_the_stored_objects_metadata() {
    let router = emulator();
    let source_bucket = "dataexchange-export-source";
    create_bucket(&router, "the source every import reads", source_bucket).await;

    // A control bucket, for the objects a CLIENT writes itself: the metadata a read reports for
    // an exported object has to be the metadata a read of the same bytes written through S3's
    // own front door reports, which is what makes the values a function of the bytes rather
    // than of who wrote them.
    let control_bucket = "dataexchange-export-control";
    create_bucket(&router, "the control objects", control_bucket).await;

    let (data_set_id, revision_id) = data_set_with_revision(&router, "exports").await;
    let asset_ids = imported_revision(&router, source_bucket, &data_set_id, &revision_id).await;

    for (row_index, row) in destination_rows().iter().enumerate() {
        let bucket = format!("dataexchange-export-destination-{row_index}");
        create_bucket(&router, row.case, &bucket).await;
        let (job_type, details) =
            export_details(row, &bucket, &data_set_id, &revision_id, &asset_ids);
        let settled = settled_job(&router, row.case, job_type, details).await;
        assert_eq!(
            settled["State"].as_str(),
            Some(COMPLETED),
            "case {:?}: an export whose destination is there must COMPLETE, got {:?} with \
             Errors {}",
            row.case,
            settled["State"],
            errors_of(&settled)
        );

        for (index, asset) in assets().iter().enumerate() {
            let case = format!("{} / {}", row.case, asset.case);
            let key = expected_key(row, index, asset, &revision_id);

            // The object the export wrote is THERE, and it carries the asset's bytes.
            let read = get_object(&router, &bucket, &key).await;
            assert_eq!(
                read.status,
                200,
                "case {case:?}: GetObject of the export destination key {key:?} must answer \
                 200, got {} with {:?}",
                read.status,
                read.text()
            );
            assert_eq!(
                read.body,
                asset.content,
                "case {case:?}: expected the exported object to carry the asset's {} bytes, \
                 got {:?}",
                asset.content.len(),
                read.text()
            );

            // ... and the metadata of the object that read served, in the headers the model
            // binds it to.
            for (header, expected) in [
                ("accept-ranges", ACCEPT_RANGES.to_string()),
                ("content-type", asset.media_type.to_string()),
                ("content-length", asset.content.len().to_string()),
                ("x-amz-server-side-encryption", SSE_S3.to_string()),
                ("x-amz-checksum-type", FULL_OBJECT.to_string()),
            ] {
                assert_eq!(
                    read.header(header),
                    Some(expected.as_str()),
                    "case {case:?}: expected {header}: {expected:?} on the read, got {:?} \
                     ({:?})",
                    read.header(header),
                    read.headers
                );
            }
            for header in ["etag", "last-modified"] {
                assert!(
                    read.header(header).is_some_and(|value| !value.is_empty()),
                    "case {case:?}: expected {header} on the read, got {:?} ({:?})",
                    read.header(header),
                    read.headers
                );
            }

            // The entity tag and the checksum describe THOSE bytes: a client's own write of
            // the same bytes, under the algorithm this read named, is stored with the same
            // pair.
            let (algorithm, digest) = reported_checksum(&case, &read);
            let control_key = format!("control/{row_index}/{index}");
            put_object(
                &router,
                &case,
                control_bucket,
                &control_key,
                &[
                    ("content-type", asset.media_type),
                    ("x-amz-sdk-checksum-algorithm", algorithm),
                ],
                asset.content,
            )
            .await;
            let control = get_object(&router, control_bucket, &control_key).await;
            assert_eq!(
                control.status, 200,
                "case {case:?}: the control object must be readable, got {} with {:?}",
                control.status,
                control.text()
            );
            assert_eq!(
                read.header("etag"),
                control.header("etag"),
                "case {case:?}: expected the exported object's ETag to be the one the same \
                 bytes are stored with, got {:?} against {:?}",
                read.header("etag"),
                control.header("etag")
            );
            let control_digest = reported_checksum(&case, &control).1;
            assert_eq!(
                digest, control_digest,
                "case {case:?}: expected the {algorithm} digest of the exported object to be \
                 the digest of those bytes, got {digest:?} against {control_digest:?}"
            );

            // And the checksum is reported only to a read that asked for it: "to retrieve the
            // checksum, this mode must be enabled" (`GetObjectRequest$ChecksumMode`).
            let unchecked = get_object_unchecked(&router, &bucket, &key).await;
            assert_eq!(
                unchecked.status,
                200,
                "case {case:?}: a read without checksum mode must still answer 200, got {} \
                 with {:?}",
                unchecked.status,
                unchecked.text()
            );
            for (_, header) in CHECKSUM_HEADER_FOR_ALGORITHM {
                assert_eq!(
                    unchecked.header(header),
                    None,
                    "case {case:?}: a read that did not enable checksum mode reports no \
                     {header}, got {:?}",
                    unchecked.header(header)
                );
            }
            assert_eq!(
                unchecked.header("x-amz-checksum-type"),
                None,
                "case {case:?}: a read that did not enable checksum mode reports no checksum \
                 type, got {:?}",
                unchecked.header("x-amz-checksum-type")
            );
        }
    }
}
