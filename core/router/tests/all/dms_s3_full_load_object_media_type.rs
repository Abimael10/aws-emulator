// SPDX-License-Identifier: CC0-1.0

//! The media type a client is told a DMS full load's output carries, across two services on
//! one engine: Database Migration Service holds the task and its endpoints, and Amazon S3
//! holds the bucket the target endpoint names and answers the reads of it.
//!
//! RULE: Amazon S3 keeps the media type a write DECLARES and reports exactly that value on
//! every later read of the object — `GetObjectOutput$ContentType` and
//! `HeadObjectOutput$ContentType` are both bound to the `Content-Type` response header, and
//! the store never infers a type from a key or from the bytes — so only a write that
//! declares none leaves the object under the store's own default for an untyped object. A
//! service that writes an object on a caller's behalf is such a writer: the type a read
//! reports for a DMS full-load file is the one the load's write declared, whatever media
//! type the source object it moved was stored under, and the generic binary media type is
//! what that write declares for an opaque data file.
//!
//! Only a cross-service suite can pin this: the task and its endpoints are DMS's, and the
//! object and the headers of a read of it are Amazon S3's, so the rule is observable only
//! with both mounted on one engine. The DMS crate's own suites reach the control plane
//! alone, and the Amazon S3 crate's own suites write their objects themselves, so they can
//! pin the store's two branches but not what a service's own write declares.
//!
//! The rows vary what the rule generalises over: the media type the SOURCE object was
//! stored under (a text type, a structured type, the generic binary type itself, a vendor
//! type with parameters, and no declared type at all), how many tables one task loads,
//! whether either endpoint carries a `BucketFolder`, and how many bytes and rows each table
//! holds. Every row also reads the two control objects a client wrote into the same target
//! bucket itself — one declaring a type, one declaring none — so the writer's declaration is
//! told apart from the store's default rather than confused with it.

use std::sync::Arc;
use std::time::Duration;

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

/// The media type a read reports for a full-load file: the generic binary media type
/// registered for an opaque stream of bytes (RFC 2046 §4.5.1, `application/octet-stream`),
/// which is what the load's own write declares for the data file it wrote.
const LOADED_MEDIA_TYPE: &str = "application/octet-stream";

/// The media type a read reports for an object whose write declared NONE — the store's own
/// default for an untyped object, which no write of this suite's loads leaves behind.
const UNTYPED_MEDIA_TYPE: &str = "binary/octet-stream";

/// The key of the control object a client writes into the target bucket DECLARING a media
/// type, which a read has to report verbatim.
const TYPED_CONTROL_KEY: &str = "controls/declared";

/// The media type that control object declares: a vendor type with a parameter, so a read
/// echoing it cannot be mistaken for either default.
const TYPED_CONTROL_MEDIA_TYPE: &str = "application/vnd.dms-suite+json; charset=utf-8";

/// The key of the control object a client writes into the target bucket declaring NO media
/// type, which a read has to report the store's default for.
const UNTYPED_CONTROL_KEY: &str = "controls/undeclared";

/// What a reader here answers for a header the response does NOT carry, so an absence is a
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

/// Write one object, declaring `media_type` where the row names one, which has to succeed.
async fn write_object(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    key: &str,
    media_type: Option<&str>,
    body: &str,
) {
    let headers: Vec<(&str, &str)> = match media_type {
        Some(media_type) => vec![("content-type", media_type)],
        None => Vec::new(),
    };
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

/// `GetObject` of one key, whatever it answers.
async fn get(router: &MultiServiceRouter, name: &str, key: &str) -> Answer {
    s3(router, "GET", &format!("/{name}/{key}"), &[], Bytes::new()).await
}

/// `HeadObject` of one key, whatever it answers.
async fn head(router: &MultiServiceRouter, name: &str, key: &str) -> Answer {
    s3(router, "HEAD", &format!("/{name}/{key}"), &[], Bytes::new()).await
}

/// The media type both reads of one key report, each read's answer asserted to be a 200
/// first so a refusal cannot pass as a missing header.
async fn reported_media_types(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    key: &str,
) -> (String, String, String) {
    let read = get(router, name, key).await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: expected GetObject({name}/{key}) to answer 200, got {} with {:?}",
        read.status,
        read.text()
    );
    let peeked = head(router, name, key).await;
    assert_eq!(
        peeked.status, 200,
        "case {case:?}: expected HeadObject({name}/{key}) to answer 200, got {}",
        peeked.status
    );
    (
        read.header("content-type"),
        peeked.header("content-type"),
        read.text(),
    )
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

/// One table a row's task is asked to load: where it lives, what the source holds for it,
/// and the media type the source object was stored under.
struct Table {
    schema: &'static str,
    table: &'static str,
    rows: &'static str,
    /// The media type the source object declares, or `None` for one that declares none —
    /// which the store keeps under its own default, so a load that echoed the source's
    /// stored type would be visible either way.
    source_media_type: Option<&'static str>,
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

/// RULE: a read reports the media type the WRITE of the object declared, verbatim, and the
/// store's own default for an untyped object only where the write declared nothing — so the
/// full-load file a DMS task wrote is reported under the generic binary media type its own
/// write declared, whatever media type the source object it moved was stored under, on both
/// `GetObject` and `HeadObject`.
#[tokio::test]
async fn a_full_loads_output_reports_the_media_type_its_write_declared() {
    let rows: &[Row] = &[
        Row {
            case: "a text source type, under a folder at either end",
            source_folder: Some("srcdata"),
            target_folder: Some("tgtdata"),
            tables: &[Table {
                schema: "hr",
                table: "employee",
                rows: "7,Mina,finance\n8,Omar,legal\n",
                source_media_type: Some("text/csv"),
            }],
        },
        Row {
            case: "three tables: a structured type, no declared type, and the generic type",
            source_folder: None,
            target_folder: None,
            tables: &[
                Table {
                    schema: "sales",
                    table: "baskets",
                    rows: "b-1,2,19.99\n",
                    source_media_type: Some("application/json"),
                },
                Table {
                    schema: "sales",
                    table: "orders",
                    rows: "o-1,2026-02-03,41.00\no-2,2026-02-04,7.25\no-3,2026-02-05,113.80\n",
                    source_media_type: None,
                },
                Table {
                    schema: "sales",
                    table: "returns",
                    rows: "r-1,o-2,7.25\n",
                    source_media_type: Some(LOADED_MEDIA_TYPE),
                },
            ],
        },
        Row {
            case: "a vendor source type with a parameter, beside a far larger table",
            source_folder: None,
            target_folder: Some("landing"),
            tables: &[
                Table {
                    schema: "stock",
                    table: "items",
                    rows: "sku-1,4",
                    source_media_type: Some("text/tab-separated-values; charset=utf-8"),
                },
                Table {
                    schema: "stock",
                    table: "movements",
                    rows: "m-01,sku-1,in,120,2026-03-01\nm-02,sku-1,out,15,2026-03-02\n\
                           m-03,sku-2,in,640,2026-03-02\nm-04,sku-3,out,7,2026-03-03\n\
                           m-05,sku-1,out,33,2026-03-04\nm-06,sku-2,in,18,2026-03-05\n",
                    source_media_type: Some("application/vnd.ms-excel"),
                },
            ],
        },
    ];

    let router = emulator();
    let instance_arn = available_instance(&router, "setup", "media-type-runner").await;

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let source_bucket = format!("dms-typed-source-{index}");
        let target_bucket = format!("dms-typed-target-{index}");
        bucket(&router, case, &source_bucket).await;
        bucket(&router, case, &target_bucket).await;

        // The two controls a CLIENT writes into the target bucket itself, which pin the two
        // branches of the store's own rule the load's write is told apart from.
        write_object(
            &router,
            case,
            &target_bucket,
            TYPED_CONTROL_KEY,
            Some(TYPED_CONTROL_MEDIA_TYPE),
            "{\"control\":\"declared\"}",
        )
        .await;
        write_object(
            &router,
            case,
            &target_bucket,
            UNTYPED_CONTROL_KEY,
            None,
            "control,undeclared\n",
        )
        .await;

        for table in row.tables {
            let key = under(
                row.source_folder,
                &format!("{}/{}/{FULL_LOAD_OBJECT}", table.schema, table.table),
            );
            write_object(
                &router,
                case,
                &source_bucket,
                &key,
                table.source_media_type,
                table.rows,
            )
            .await;
            // What the SOURCE object itself reports, so each row's source side is the media
            // type the row names rather than an assumption about it.
            let (served, peeked, _) =
                reported_media_types(&router, case, &source_bucket, &key).await;
            let expected = table.source_media_type.unwrap_or(UNTYPED_MEDIA_TYPE);
            assert_eq!(
                (served.as_str(), peeked.as_str()),
                (expected, expected),
                "case {case:?}: expected the source object at {key} to report the media type \
                 its own write declared, got {served:?} on a read and {peeked:?} on a head"
            );
        }

        let source_arn = s3_endpoint(
            &router,
            case,
            &format!("typed-src-{index}"),
            "source",
            &source_bucket,
            row.source_folder,
        )
        .await;
        let target_arn = s3_endpoint(
            &router,
            case,
            &format!("typed-tgt-{index}"),
            "target",
            &target_bucket,
            row.target_folder,
        )
        .await;
        let task_arn = ready_task(
            &router,
            case,
            &format!("typed-task-{index}"),
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

        // ---- what a read of the load's output reports ------------------------------------
        for table in row.tables {
            let key = under(
                row.target_folder,
                &format!("{}/{}/{FULL_LOAD_OBJECT}", table.schema, table.table),
            );
            let (served, peeked, body) =
                reported_media_types(&router, case, &target_bucket, &key).await;
            assert_eq!(
                (served.as_str(), peeked.as_str()),
                (LOADED_MEDIA_TYPE, LOADED_MEDIA_TYPE),
                "case {case:?}: expected both reads of the loaded object at {key} to report \
                 the media type the load's write declared, got {served:?} on a read and \
                 {peeked:?} on a head, for a source object stored as {:?}",
                table.source_media_type.unwrap_or(UNTYPED_MEDIA_TYPE)
            );
            assert_eq!(
                body, table.rows,
                "case {case:?}: expected the loaded object at {key} to carry the rows the \
                 source held, got {body:?}"
            );
        }

        // ---- and what the two controls in the same bucket still report --------------------
        let (typed_read, typed_head, _) =
            reported_media_types(&router, case, &target_bucket, TYPED_CONTROL_KEY).await;
        assert_eq!(
            (typed_read.as_str(), typed_head.as_str()),
            (TYPED_CONTROL_MEDIA_TYPE, TYPED_CONTROL_MEDIA_TYPE),
            "case {case:?}: expected a declared media type to be reported verbatim, got \
             {typed_read:?} on a read and {typed_head:?} on a head"
        );
        let (untyped_read, untyped_head, _) =
            reported_media_types(&router, case, &target_bucket, UNTYPED_CONTROL_KEY).await;
        assert_eq!(
            (untyped_read.as_str(), untyped_head.as_str()),
            (UNTYPED_MEDIA_TYPE, UNTYPED_MEDIA_TYPE),
            "case {case:?}: expected a write that declared no media type to leave the object \
             under the store's own default, got {untyped_read:?} on a read and \
             {untyped_head:?} on a head"
        );
    }
}
