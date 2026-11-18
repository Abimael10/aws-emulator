// SPDX-License-Identifier: CC0-1.0

//! What a DMS full load's output looks like when a client LISTS it, across two services on
//! one engine: Database Migration Service holds the task and its endpoints, and Amazon S3
//! holds the bucket the target endpoint names and answers the listing of it.
//!
//! RULE: the objects a bucket holds under a prefix are a function of what was WRITTEN to
//! that bucket, so `ListObjectsV2` over a prefix answers one `Contents` entry per key stored
//! under it — never none where the store holds one — with `KeyCount` equal to the number of
//! entries that answer carries, and each entry's `Key`, `LastModified`, `ETag`, `Size`,
//! `StorageClass`, `ChecksumAlgorithm` and `ChecksumType` read off the stored object, so a
//! listing and a read of the same key cannot disagree. A completed full load of a table at
//! an Amazon S3 target endpoint is one such write — a single-part write of that table's rows
//! under the endpoint's own bucket folder — and is listed exactly like any other; a table the
//! load moved nothing for is the only reason that prefix lists nothing.
//!
//! Only a cross-service suite can pin this: the task and its endpoints are DMS's, and the
//! bucket, the objects and the listing are Amazon S3's, so the rule is observable only with
//! both mounted on one engine. The DMS crate's own suites reach the control plane alone, and
//! the Amazon S3 crate's listing suites write their objects themselves.
//!
//! The rows vary what the rule generalises over: how many tables one task loads, whether
//! either endpoint carries a `BucketFolder`, how many bytes and rows each table holds,
//! whether a table's last row ends with a line break, and a selected table the source holds
//! nothing for. Each row reads its listing three ways — one table's prefix, the whole
//! schema's prefix, and the schema's prefix rolled up by a delimiter — since `KeyCount`
//! counts the entries of the answer, each rolled-up prefix counting as one.

use std::sync::Arc;
use std::time::Duration;

use aws_smithy_types::date_time::{DateTime, Format};
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The long-term key every emulator publishes, which is what these calls are signed with.
/// The signature itself is never checked: this router runs with request authentication off,
/// as every suite in this crate runs it.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The `X-Amz-Target` prefix of DMS, which is its Smithy service shape name.
const DMS_TARGET_PREFIX: &str = "AmazonDMSv20160101";

/// The one full-load object a table's rows arrive at an Amazon S3 target as: DMS names a
/// full-load file `LOAD` followed by its sequence number, and one load of one table writes
/// the first and only file of that sequence.
const FULL_LOAD_OBJECT: &str = "LOAD00000001.csv";

/// The storage class an object written with no class of its own is stored under: "the default
/// storage class … is STANDARD" (`PutObjectRequest$StorageClass`), which is what
/// `Object$StorageClass` then reports for it.
const STANDARD: &str = "STANDARD";

/// The checksum algorithm Amazon S3 applies to a write that named none — "if it was uploaded
/// without a checksum (and Amazon S3 added the default checksum, CRC64NVME, to the uploaded
/// object)" (`PutObjectOutput$ChecksumCRC64NVME`) — which is what `Object$ChecksumAlgorithm`
/// then reports for it.
const DEFAULT_CHECKSUM_ALGORITHM: &str = "CRC64NVME";

/// The checksum type of an object a single write stored whole: "for `PutObject` uploads, the
/// checksum type is always FULL_OBJECT" (`PutObjectRequest$ChecksumType`).
const FULL_OBJECT: &str = "FULL_OBJECT";

/// The response header a read reports the stored `CRC64NVME` checksum in
/// (`GetObjectOutput$ChecksumCRC64NVME` is bound to it).
const CRC64NVME_HEADER: &str = "x-amz-checksum-crc64nvme";

/// The response header a read reports the stored checksum's type in
/// (`GetObjectOutput$ChecksumType`).
const CHECKSUM_TYPE_HEADER: &str = "x-amz-checksum-type";

/// What a reader here answers for a header or an element the answer does NOT carry, so an
/// absence is a value no comparison can mistake for the thing itself.
const MISSING: &str = "<missing>";

/// How long one poll of a clock-owed transition waits before asking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long a poll loop waits for a clock-owed transition before giving up. Far past every
/// span this emulator gives one, so a slow machine does not fail a case the clock would have
/// settled.
const POLL_BUDGET: Duration = Duration::from_secs(120);

/// The two services this rule spans, over ONE engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_databasemigrationservice_amazondmsv20160101::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={AMBIENT_KEY}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// What one call answered.
struct Answer {
    status: u16,
    headers: http::HeaderMap,
    body: Bytes,
}

impl Answer {
    /// The body as text.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// The body as JSON, `Null` when it is neither JSON nor present.
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    /// One response header, or a marker naming its absence.
    fn header(&self, name: &str) -> String {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or(MISSING)
            .to_string()
    }
}

/// Dispatch one request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answer {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .dispatch(builder.body(body).expect("a well-formed request"))
        .await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// One DMS call, as an awsJson1_1 client makes it.
async fn dms(router: &MultiServiceRouter, operation: &str, body: &Value) -> Answer {
    let target = format!("{DMS_TARGET_PREFIX}.{operation}");
    dispatch(
        router,
        "POST",
        &format!("dms.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", &target),
            ("authorization", &authorization("dms")),
        ],
        Bytes::from(body.to_string()),
    )
    .await
}

/// One DMS call that has to succeed, answering its body as JSON.
async fn dms_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let answer = dms(router, operation, body).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected {operation} to answer 200, got {} with {:?}",
        answer.status,
        answer.text()
    );
    answer.json()
}

/// One Amazon S3 REST call, path-style as the emulator serves it.
async fn s3(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answer {
    let mut with_credential: Vec<(&str, &str)> = headers.to_vec();
    let credential = authorization("s3");
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

/// Create one bucket, which has to succeed.
async fn bucket(router: &MultiServiceRouter, case: &str, name: &str) {
    let created = s3(router, "PUT", &format!("/{name}"), &[], Bytes::new()).await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: CreateBucket({name}) must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
}

/// Stage one source object, which has to succeed.
async fn stage(router: &MultiServiceRouter, case: &str, name: &str, key: &str, body: &str) {
    let written = s3(
        router,
        "PUT",
        &format!("/{name}/{key}"),
        &[("content-type", "text/csv")],
        Bytes::from(body.to_string()),
    )
    .await;
    assert_eq!(
        written.status, 200,
        "case {case:?}: PutObject({name}/{key}) must succeed, got {} with {:?}",
        written.status,
        written.text()
    );
}

/// `ListObjectsV2` over `query` of `name`, which has to succeed, as its XML text.
async fn listing(router: &MultiServiceRouter, case: &str, name: &str, query: &str) -> String {
    let listed = s3(
        router,
        "GET",
        &format!("/{name}?list-type=2{query}"),
        &[],
        Bytes::new(),
    )
    .await;
    assert_eq!(
        listed.status, 200,
        "case {case:?}: ListObjectsV2({name}?{query}) must succeed, got {} with {:?}",
        listed.status,
        listed.text()
    );
    listed.text()
}

/// `HeadObject` of one key with its checksum reported, which has to succeed.
async fn head(router: &MultiServiceRouter, case: &str, name: &str, key: &str) -> Answer {
    let answer = s3(
        router,
        "HEAD",
        &format!("/{name}/{key}"),
        &[("x-amz-checksum-mode", "ENABLED")],
        Bytes::new(),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: HeadObject({name}/{key}) must succeed, got {}",
        answer.status
    );
    answer
}

/// Every `<name>…</name>` span of an XML body, body text only — how an Amazon S3 listing is
/// read here, the same way the sibling full-load suite reads one.
fn xml_spans(body: &str, name: &str) -> Vec<String> {
    let (open, close) = (format!("<{name}>"), format!("</{name}>"));
    let mut found = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find(&open) {
        let from = start + open.len();
        let Some(end) = rest[from..].find(&close) else {
            break;
        };
        found.push(rest[from..from + end].to_string());
        rest = &rest[from + end + close.len()..];
    }
    found
}

/// The first `<name>…</name>` span of an XML body, or a marker naming its absence.
fn xml_span(body: &str, name: &str) -> String {
    xml_spans(body, name)
        .into_iter()
        .next()
        .unwrap_or_else(|| MISSING.to_string())
}

/// The text of one XML element, with the entity escapes an Amazon S3 listing writes
/// resolved: an `ETag` is a quoted hash, and XML escapes the quotation marks it carries.
fn xml_text(body: &str, name: &str) -> String {
    xml_span(body, name)
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
}

/// A replication instance that has finished provisioning, and its ARN.
async fn available_instance(router: &MultiServiceRouter, case: &str, identifier: &str) -> String {
    let created = dms_ok(
        router,
        case,
        "CreateReplicationInstance",
        &json!({
            "ReplicationInstanceIdentifier": identifier,
            "ReplicationInstanceClass": "dms.t3.medium",
        }),
    )
    .await;
    let arn = text(&created, "/ReplicationInstance/ReplicationInstanceArn");
    let deadline = tokio::time::Instant::now() + POLL_BUDGET;
    loop {
        let described = dms_ok(
            router,
            case,
            "DescribeReplicationInstances",
            &json!({
                "Filters": [{ "Name": "replication-instance-arn", "Values": [arn] }],
            }),
        )
        .await;
        let status = text(&described, "/ReplicationInstances/0/ReplicationInstanceStatus");
        if status == "available" {
            return arn;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: the replication instance never reached available, last {status:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// An s3 endpoint over `name`/`folder`, and its ARN.
async fn s3_endpoint(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    endpoint_type: &str,
    name: &str,
    folder: Option<&str>,
) -> String {
    let mut settings = json!({ "BucketName": name });
    if let Some(folder) = folder {
        settings["BucketFolder"] = json!(folder);
    }
    let created = dms_ok(
        router,
        case,
        "CreateEndpoint",
        &json!({
            "EndpointIdentifier": identifier,
            "EndpointType": endpoint_type,
            "EngineName": "s3",
            "S3Settings": settings,
        }),
    )
    .await;
    text(&created, "/Endpoint/EndpointArn")
}

/// One task, as `DescribeReplicationTasks` reports it.
async fn described_task(router: &MultiServiceRouter, case: &str, task_arn: &str) -> Value {
    let described = dms_ok(
        router,
        case,
        "DescribeReplicationTasks",
        &json!({
            "Filters": [{ "Name": "replication-task-arn", "Values": [task_arn] }],
        }),
    )
    .await;
    described
        .pointer("/ReplicationTasks/0")
        .cloned()
        .unwrap_or(Value::Null)
}

/// A replication task that has finished being created, and its ARN.
async fn ready_task(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    instance_arn: &str,
    source_arn: &str,
    target_arn: &str,
    table_mappings: &str,
) -> String {
    let created = dms_ok(
        router,
        case,
        "CreateReplicationTask",
        &json!({
            "ReplicationTaskIdentifier": identifier,
            "SourceEndpointArn": source_arn,
            "TargetEndpointArn": target_arn,
            "ReplicationInstanceArn": instance_arn,
            "MigrationType": "full-load",
            "TableMappings": table_mappings,
        }),
    )
    .await;
    let arn = text(&created, "/ReplicationTask/ReplicationTaskArn");
    let deadline = tokio::time::Instant::now() + POLL_BUDGET;
    loop {
        let status = text(&described_task(router, case, &arn).await, "/Status");
        if status == "ready" {
            return arn;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: the replication task never reached ready, last {status:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll the task until its run's full load reports itself complete.
async fn await_loaded(router: &MultiServiceRouter, case: &str, task_arn: &str) {
    let deadline = tokio::time::Instant::now() + POLL_BUDGET;
    loop {
        let task = described_task(router, case, task_arn).await;
        if task
            .pointer("/ReplicationTaskStats/FullLoadProgressPercent")
            .and_then(Value::as_i64)
            == Some(100)
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: the task's full load never reported itself complete, last {task}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The string at `pointer` in `value`, or a marker naming its absence.
fn text(value: &Value, pointer: &str) -> String {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or(MISSING)
        .to_string()
}

/// One table a row's task is asked to load: where it lives and what the source holds for it.
struct Table {
    schema: &'static str,
    table: &'static str,
    /// The rows the source holds for it, or `None` for a table the source holds nothing for.
    rows: Option<&'static str>,
}

/// One row of the rule: a full-load task over one pair of Amazon S3 endpoints.
struct Row {
    case: &'static str,
    source_folder: Option<&'static str>,
    target_folder: Option<&'static str>,
    tables: &'static [Table],
}

/// A table-mappings document selecting every table of `tables` outright.
fn table_mappings(tables: &[Table]) -> String {
    let rules: Vec<Value> = tables
        .iter()
        .enumerate()
        .map(|(index, table)| {
            json!({
                "rule-type": "selection",
                "rule-id": (index + 1).to_string(),
                "rule-name": (index + 1).to_string(),
                "object-locator": { "schema-name": table.schema, "table-name": table.table },
                "rule-action": "include",
            })
        })
        .collect();
    json!({ "rules": rules }).to_string()
}

/// `prefix` with `folder` in front of it, where the endpoint carries one.
fn under(folder: Option<&str>, rest: &str) -> String {
    match folder {
        Some(folder) => format!("{folder}/{rest}"),
        None => rest.to_string(),
    }
}

/// RULE: `ListObjectsV2` over the prefix a completed full load wrote to answers one
/// `Contents` entry per object the target bucket holds there — `KeyCount` counting the
/// entries of the answer — with each entry's members read off the stored object, so the
/// listing agrees with a read of the same key; a prefix the load wrote nothing under, and
/// only such a prefix, lists no keys at all.
#[tokio::test]
async fn a_finished_full_loads_objects_are_listed_at_the_target_prefix() {
    let rows: &[Row] = &[
        Row {
            case: "one table, under a folder at either end",
            source_folder: Some("srcdata"),
            target_folder: Some("tgtdata"),
            tables: &[Table {
                schema: "hr",
                table: "employee",
                rows: Some("7,Mina,finance\n8,Omar,legal\n"),
            }],
        },
        Row {
            case: "three selected tables, no folder at either end, one absent at the source",
            source_folder: None,
            target_folder: None,
            tables: &[
                Table {
                    schema: "sales",
                    table: "baskets",
                    rows: Some("b-1,2,19.99\n"),
                },
                Table {
                    schema: "sales",
                    table: "orders",
                    rows: Some(
                        "o-1,2026-02-03,41.00\no-2,2026-02-04,7.25\no-3,2026-02-05,113.80\n",
                    ),
                },
                Table {
                    schema: "sales",
                    table: "returns",
                    rows: None,
                },
            ],
        },
        Row {
            case: "a table whose last row carries no line break, beside a far larger one",
            source_folder: None,
            target_folder: Some("landing"),
            tables: &[
                Table {
                    schema: "stock",
                    table: "items",
                    rows: Some("sku-1,4"),
                },
                Table {
                    schema: "stock",
                    table: "movements",
                    rows: Some(
                        "m-01,sku-1,in,120,2026-03-01\nm-02,sku-1,out,15,2026-03-02\n\
                         m-03,sku-2,in,640,2026-03-02\nm-04,sku-3,out,7,2026-03-03\n\
                         m-05,sku-1,out,33,2026-03-04\nm-06,sku-2,in,18,2026-03-05\n",
                    ),
                },
            ],
        },
    ];

    let router = emulator();
    let instance_arn = available_instance(&router, "setup", "listing-runner").await;

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let source_bucket = format!("dms-listed-source-{index}");
        let target_bucket = format!("dms-listed-target-{index}");
        bucket(&router, case, &source_bucket).await;
        bucket(&router, case, &target_bucket).await;
        for table in row.tables {
            let Some(rows) = table.rows else { continue };
            let key = under(
                row.source_folder,
                &format!("{}/{}/{FULL_LOAD_OBJECT}", table.schema, table.table),
            );
            stage(&router, case, &source_bucket, &key, rows).await;
        }

        let source_arn = s3_endpoint(
            &router,
            case,
            &format!("listed-src-{index}"),
            "source",
            &source_bucket,
            row.source_folder,
        )
        .await;
        let target_arn = s3_endpoint(
            &router,
            case,
            &format!("listed-tgt-{index}"),
            "target",
            &target_bucket,
            row.target_folder,
        )
        .await;
        let task_arn = ready_task(
            &router,
            case,
            &format!("listed-task-{index}"),
            &instance_arn,
            &source_arn,
            &target_arn,
            &table_mappings(row.tables),
        )
        .await;
        let started = dms_ok(
            &router,
            case,
            "StartReplicationTask",
            &json!({
                "ReplicationTaskArn": task_arn,
                "StartReplicationTaskType": "start-replication",
            }),
        )
        .await;
        assert_eq!(
            text(&started, "/ReplicationTask/Status"),
            "starting",
            "case {case:?}: expected a started task to report starting, got {started}"
        );
        await_loaded(&router, case, &task_arn).await;

        // ---- one table's own prefix ---------------------------------------------------
        for table in row.tables {
            let prefix = under(
                row.target_folder,
                &format!("{}/{}/", table.schema, table.table),
            );
            let body = listing(&router, case, &target_bucket, &format!("&prefix={prefix}")).await;
            let entries = xml_spans(&body, "Contents");
            let key_count = xml_span(&body, "KeyCount");

            let Some(rows) = table.rows else {
                // The ONLY reason a prefix lists nothing: the load wrote nothing there.
                assert_eq!(
                    (key_count.as_str(), entries.len()),
                    ("0", 0),
                    "case {case:?}: expected no keys under {prefix} for a table the source \
                     holds nothing for, got {body:?}"
                );
                continue;
            };

            assert_eq!(
                (key_count.as_str(), entries.len()),
                ("1", 1),
                "case {case:?}: expected one listed key under {prefix}, got {body:?}"
            );
            let entry = entries.first().cloned().unwrap_or_default();
            let key = format!("{prefix}{FULL_LOAD_OBJECT}");
            let stored = head(&router, case, &target_bucket, &key).await;

            // What the STORED object reports, which the listed entry is then compared
            // against: each is read off the response rather than from the absence marker, so
            // a member neither answer carries fails here rather than matching the other's
            // absence.
            let stored_etag = stored.header("etag");
            let stored_checksum = stored.header(CRC64NVME_HEADER);
            assert!(
                stored_etag.starts_with('"') && stored_etag.ends_with('"') && stored_etag.len() > 2,
                "case {case:?}: expected HeadObject({key}) to report a quoted entity tag, got \
                 {stored_etag:?} among {:?}",
                stored.headers
            );
            assert!(
                stored_checksum != MISSING && !stored_checksum.is_empty(),
                "case {case:?}: expected HeadObject({key}) to report the default CRC64NVME \
                 checksum of the loaded bytes, got {stored_checksum:?} among {:?}",
                stored.headers
            );

            assert_eq!(
                (
                    xml_span(&entry, "Key"),
                    xml_span(&entry, "Size"),
                    xml_span(&entry, "StorageClass"),
                    xml_text(&entry, "ETag"),
                    xml_span(&entry, "ChecksumAlgorithm"),
                    xml_span(&entry, "ChecksumType"),
                ),
                (
                    key.clone(),
                    rows.len().to_string(),
                    STANDARD.to_string(),
                    stored_etag.clone(),
                    DEFAULT_CHECKSUM_ALGORITHM.to_string(),
                    FULL_OBJECT.to_string(),
                ),
                "case {case:?}: expected the listed entry of {key} to report the stored \
                 object's key, size, storage class, entity tag and checksum, got {entry:?} \
                 beside HEAD {:?}",
                stored.headers
            );
            assert_eq!(
                (
                    stored.header("content-length"),
                    stored.header(CHECKSUM_TYPE_HEADER),
                ),
                (rows.len().to_string(), FULL_OBJECT.to_string()),
                "case {case:?}: expected the stored object at {key} to carry the loaded \
                 bytes under a full-object checksum, got {:?}",
                stored.headers
            );
            let last_modified = xml_span(&entry, "LastModified");
            assert!(
                DateTime::from_str(&last_modified, Format::DateTime).is_ok(),
                "case {case:?}: expected the listed entry of {key} to report a creation date \
                 as a date-time, got {last_modified:?}"
            );
        }

        // ---- every table of the schema, listed at once ---------------------------------
        let schema = row.tables.first().map(|table| table.schema).unwrap_or("");
        let schema_prefix = under(row.target_folder, &format!("{schema}/"));
        let loaded: Vec<String> = row
            .tables
            .iter()
            .filter(|table| table.rows.is_some())
            .map(|table| format!("{schema_prefix}{}/{FULL_LOAD_OBJECT}", table.table))
            .collect();

        let body = listing(
            &router,
            case,
            &target_bucket,
            &format!("&prefix={schema_prefix}"),
        )
        .await;
        assert_eq!(
            (
                xml_span(&body, "KeyCount"),
                xml_spans(&body, "Contents")
                    .iter()
                    .map(|entry| xml_span(entry, "Key"))
                    .collect::<Vec<String>>(),
            ),
            (loaded.len().to_string(), loaded.clone()),
            "case {case:?}: expected one entry per loaded table under {schema_prefix}, \
             counted by KeyCount, got {body:?}"
        );

        // A rolled-up listing returns no keys of its own, and each prefix it rolls up
        // counts as one entry of the answer.
        let folded = listing(
            &router,
            case,
            &target_bucket,
            &format!("&prefix={schema_prefix}&delimiter=%2F"),
        )
        .await;
        let folded_prefixes: Vec<String> = xml_spans(&folded, "CommonPrefixes")
            .iter()
            .map(|entry| xml_span(entry, "Prefix"))
            .collect();
        let expected_prefixes: Vec<String> = row
            .tables
            .iter()
            .filter(|table| table.rows.is_some())
            .map(|table| format!("{schema_prefix}{}/", table.table))
            .collect();
        assert_eq!(
            (
                xml_span(&folded, "KeyCount"),
                folded_prefixes,
                xml_spans(&folded, "Contents").len(),
            ),
            (
                expected_prefixes.len().to_string(),
                expected_prefixes.clone(),
                0,
            ),
            "case {case:?}: expected {schema_prefix} rolled up by a delimiter to report one \
             common prefix per loaded table and no keys, got {folded:?}"
        );
    }
}
