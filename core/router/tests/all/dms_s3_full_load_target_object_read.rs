// SPDX-License-Identifier: CC0-1.0

//! What a DMS full load's output looks like when a client READS it, across two services on
//! one engine: Database Migration Service holds the task and its endpoints, and Amazon S3
//! holds the bucket the target endpoint names and serves the object out of it.
//!
//! RULE: a write that DECLARES the media type of the bytes it carries leaves an object whose
//! every later read reports that declared type, so the one full-load file a table's rows
//! arrive at an Amazon S3 target as — written as the opaque byte stream the load assembled,
//! under `application/octet-stream` — is served with that type, its stored bytes, and the
//! members Amazon S3 reports for any single-part write beside them (a range unit of `bytes`,
//! the entity tag of the stored bytes, the date they were stored and the encryption they are
//! stored under); only a key the store genuinely does not hold is refused.
//!
//! Only a cross-service suite can pin this: the task, its endpoints and the write are DMS's,
//! and the bucket, the object and the read are Amazon S3's, so the rule is observable only
//! with both mounted on one engine. The DMS crate's own suites reach the control plane alone,
//! and the Amazon S3 crate's read suites write their objects themselves.
//!
//! The rows vary what the rule generalises over: how many tables one task loads, whether
//! either endpoint carries a `BucketFolder`, how many bytes and rows each table holds,
//! whether a table's last row ends with a line break, what media type the SOURCE objects were
//! staged under (including none at all, and one that is not the loaded file's), and a selected
//! table the source holds nothing for — which is the only key the target refuses.

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

/// The media type a full-load file is written and therefore served under: the media type of
/// an undifferentiated stream of bytes, which is what the load declares for the rows it
/// assembled.
const FULL_LOAD_MEDIA_TYPE: &str = "application/octet-stream";

/// The range unit Amazon S3 reports it accepts for a stored object
/// (`GetObjectOutput$AcceptRanges`, bound to `accept-ranges`).
const BYTES: &str = "bytes";

/// The server-side encryption an object stored with no key of the caller's is encrypted
/// under: "Amazon S3 now applies server-side encryption with Amazon S3 managed keys (SSE-S3)
/// … as the base level of encryption" (`GetObjectOutput$ServerSideEncryption` reports it),
/// whose algorithm value is this one.
const AES256: &str = "AES256";

/// The error code Amazon S3 refuses a read of a key it does not hold with
/// (`NoSuchKey`, HTTP 404).
const NO_SUCH_KEY: &str = "NoSuchKey";

/// What a reader here answers for a header the answer does NOT carry, so an absence is a
/// value no comparison can mistake for the thing itself.
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

/// Stage one source object under `media_type`, where the staging client declares one, which
/// has to succeed.
async fn stage(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    key: &str,
    body: &str,
    media_type: Option<&str>,
) {
    let headers: Vec<(&str, &str)> = media_type
        .map(|media_type| vec![("content-type", media_type)])
        .unwrap_or_default();
    let written = s3(
        router,
        "PUT",
        &format!("/{name}/{key}"),
        &headers,
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

/// The text of the first `<name>…</name>` element of an XML body, or a marker naming its
/// absence — how an Amazon S3 refusal is read here.
fn xml_span(body: &str, name: &str) -> String {
    let (open, close) = (format!("<{name}>"), format!("</{name}>"));
    let Some(start) = body.find(&open) else {
        return MISSING.to_string();
    };
    let from = start + open.len();
    match body[from..].find(&close) {
        Some(end) => body[from..from + end].to_string(),
        None => MISSING.to_string(),
    }
}

/// One table a row's task is asked to load: where it lives, what the source holds for it, and
/// the media type the source objects were staged under.
struct Table {
    schema: &'static str,
    table: &'static str,
    /// The rows the source holds for it, or `None` for a table the source holds nothing for.
    rows: Option<&'static str>,
    /// The `Content-Type` the source object was staged with, or `None` for a staging client
    /// that declared none.
    staged_as: Option<&'static str>,
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

/// `rest` with `folder` in front of it, where the endpoint carries one.
fn under(folder: Option<&str>, rest: &str) -> String {
    match folder {
        Some(folder) => format!("{folder}/{rest}"),
        None => rest.to_string(),
    }
}

/// RULE: the full-load file a completed load wrote to the target bucket is SERVED — HTTP 200
/// carrying the loaded bytes, under the media type the write declared for them
/// (`application/octet-stream`, whatever media type the source objects were staged under),
/// beside the range unit, entity tag, storage date, length and encryption Amazon S3 reports
/// for any single-part write; a read of the same key twice agrees with itself, and the only
/// key refused `NoSuchKey` is one the store genuinely does not hold.
#[tokio::test]
async fn a_finished_full_loads_object_is_served_with_the_declared_media_type() {
    let rows: &[Row] = &[
        Row {
            case: "one table, under a folder at either end, staged as a delimited text file",
            source_folder: Some("srcdata"),
            target_folder: Some("tgtdata"),
            tables: &[Table {
                schema: "hr",
                table: "employee",
                rows: Some("4,Ida,eng\n5,Bo,ops\n6,Yara,eng\n"),
                staged_as: Some("text/csv"),
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
                    // A staging client that declared no media type at all.
                    staged_as: None,
                },
                Table {
                    schema: "sales",
                    table: "orders",
                    rows: Some("o-1,2026-02-03,41.00\no-2,2026-02-04,7.25\n"),
                    // A media type that is neither the loaded file's nor a text one.
                    staged_as: Some("application/json"),
                },
                Table {
                    schema: "sales",
                    table: "returns",
                    rows: None,
                    staged_as: None,
                },
            ],
        },
        Row {
            case: "a one-row table whose row carries no line break, beside a far larger one",
            source_folder: None,
            target_folder: Some("landing"),
            tables: &[
                Table {
                    schema: "stock",
                    table: "items",
                    rows: Some("sku-1,4"),
                    staged_as: Some("text/plain"),
                },
                Table {
                    schema: "stock",
                    table: "movements",
                    rows: Some(
                        "m-01,sku-1,in,120,2026-03-01\nm-02,sku-1,out,15,2026-03-02\n\
                         m-03,sku-2,in,640,2026-03-02\nm-04,sku-3,out,7,2026-03-03\n\
                         m-05,sku-1,out,33,2026-03-04\nm-06,sku-2,in,18,2026-03-05\n",
                    ),
                    staged_as: Some("text/csv"),
                },
            ],
        },
    ];

    let router = emulator();
    let instance_arn = available_instance(&router, "setup", "read-runner").await;

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let source_bucket = format!("dms-read-source-{index}");
        let target_bucket = format!("dms-read-target-{index}");
        bucket(&router, case, &source_bucket).await;
        bucket(&router, case, &target_bucket).await;
        for table in row.tables {
            let Some(staged) = table.rows else { continue };
            let key = under(
                row.source_folder,
                &format!("{}/{}/{FULL_LOAD_OBJECT}", table.schema, table.table),
            );
            stage(
                &router,
                case,
                &source_bucket,
                &key,
                staged,
                table.staged_as,
            )
            .await;
        }

        let source_arn = s3_endpoint(
            &router,
            case,
            &format!("read-src-{index}"),
            "source",
            &source_bucket,
            row.source_folder,
        )
        .await;
        let target_arn = s3_endpoint(
            &router,
            case,
            &format!("read-tgt-{index}"),
            "target",
            &target_bucket,
            row.target_folder,
        )
        .await;
        let task_arn = ready_task(
            &router,
            case,
            &format!("read-task-{index}"),
            &instance_arn,
            &source_arn,
            &target_arn,
            &table_mappings(row.tables),
        )
        .await;
        dms_ok(
            &router,
            case,
            "StartReplicationTask",
            &json!({
                "ReplicationTaskArn": task_arn,
                "StartReplicationTaskType": "start-replication",
            }),
        )
        .await;
        await_loaded(&router, case, &task_arn).await;

        for table in row.tables {
            let key = under(
                row.target_folder,
                &format!("{}/{}/{FULL_LOAD_OBJECT}", table.schema, table.table),
            );
            let read = s3(&router, "GET", &format!("/{target_bucket}/{key}"), &[], Bytes::new())
                .await;

            let Some(loaded) = table.rows else {
                // The ONLY key refused: one the store genuinely does not hold, because the
                // load had nothing to write there.
                assert_eq!(
                    (read.status, xml_span(&read.text(), "Code")),
                    (404, NO_SUCH_KEY.to_string()),
                    "case {case:?}: expected a read of {key}, which no load wrote, to be \
                     refused 404 {NO_SUCH_KEY}, got {} with {:?}",
                    read.status,
                    read.text()
                );
                continue;
            };

            assert_eq!(
                (read.status, read.text()),
                (200, loaded.to_string()),
                "case {case:?}: expected a read of {key} to serve the loaded rows, got {} \
                 with {:?} among {:?}",
                read.status,
                read.text(),
                read.headers
            );
            assert_eq!(
                (
                    read.header("content-type"),
                    read.header("accept-ranges"),
                    read.header("x-amz-server-side-encryption"),
                    read.header("content-length"),
                ),
                (
                    FULL_LOAD_MEDIA_TYPE.to_string(),
                    BYTES.to_string(),
                    AES256.to_string(),
                    loaded.len().to_string(),
                ),
                "case {case:?}: expected the read of {key} to report the media type the load \
                 declared (not the source object's {:?}), the range unit, the encryption and \
                 the stored length, got {:?}",
                table.staged_as,
                read.headers
            );
            let etag = read.header("etag");
            assert!(
                etag.starts_with('"') && etag.ends_with('"') && etag.len() > 2,
                "case {case:?}: expected the read of {key} to report a quoted entity tag, got \
                 {etag:?} among {:?}",
                read.headers
            );
            let last_modified = read.header("last-modified");
            assert!(
                DateTime::from_str(&last_modified, Format::HttpDate).is_ok(),
                "case {case:?}: expected the read of {key} to report the date its bytes were \
                 stored as an HTTP date, got {last_modified:?}"
            );

            // A read changes nothing a later reader sees: the same key, read again, answers
            // the same bytes under the same reported members.
            let again = s3(&router, "GET", &format!("/{target_bucket}/{key}"), &[], Bytes::new())
                .await;
            assert_eq!(
                (
                    again.status,
                    again.text(),
                    again.header("content-type"),
                    again.header("etag"),
                    again.header("last-modified"),
                ),
                (
                    200,
                    loaded.to_string(),
                    FULL_LOAD_MEDIA_TYPE.to_string(),
                    etag.clone(),
                    last_modified.clone(),
                ),
                "case {case:?}: expected a second read of {key} to agree with the first, got \
                 {} with {:?} among {:?}",
                again.status,
                again.text(),
                again.headers
            );
        }
    }
}
