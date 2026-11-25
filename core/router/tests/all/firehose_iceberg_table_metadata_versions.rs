// SPDX-License-Identifier: CC0-1.0

//! The METADATA VERSIONS of the Apache Iceberg table a Firehose stream delivers into: the
//! version its catalog create minted, the version each flush commits, and what the versions
//! already written keep.
//!
//! RULE: an Iceberg table is read through the metadata file its catalog row points at, so a
//! table is only usable when that pointer resolves and every version of it stays where it was
//! written. Creating the table with an `OpenTableFormatInput$IcebergInput` writes version zero
//! of the document — the format version the input asked for ("The table version for the
//! Iceberg table. Defaults to 2", `IcebergInput$Version`), the table's own location, one schema
//! whose fields are the `StorageDescriptor$Columns` in declaration order, and no snapshot at
//! all — and every flush an `IcebergDestinationConfiguration` destination commits appends ONE
//! snapshot by writing the NEXT numbered version beside the versions already there, then moves
//! the row's `metadata_location` to it. So, after any number of flushes, a version's ordinal is
//! the number of snapshots it holds, the table's current version is the newest ordinal present,
//! every earlier version is still readable under the same table identity, and the current
//! document's snapshots account — through `added-records` — for exactly the records the
//! producer published, whatever feeds the stream.
//!
//! RULE: server-side encryption of a Firehose stream applies to the direct-put path alone, so
//! `DescribeDeliveryStream` carries `DeliveryStreamEncryptionConfiguration` for a `DirectPut`
//! stream and leaves the member out for a stream fed by a configured source, whose SSE state
//! does not exist to report.
//!
//! RULE: an Iceberg destination's tables are resolved in the Data Catalog by the call that
//! submits the configuration, so a create naming a table — or a database — the catalog cannot
//! serve is refused with `InvalidArgumentException` and registers no stream.
//!
//! Only a cross-service suite can pin these on the path a client drives: the flow spans the
//! five services `core/router/src/lib.rs` mounts on ONE engine — Kinesis Data Streams for the
//! source a stream consumes, IAM for the delivery role the catalog read and the object write
//! are made under, Glue for the Data Catalog the destination table lives in, Firehose for the
//! streams, and Amazon S3 for the objects a commit writes.
//!
//! The rows differ along what the rules generalise over: what feeds the stream (a Kinesis data
//! stream or direct put) and therefore how the producer publishes (`PutRecords` or
//! `PutRecordBatch`), how many flushes the producer drives and how many records each one
//! carries, the column types the destination table declares, and — for the refusal — which half
//! of the catalog target is missing.
//!
//! Nothing here asserts a value this emulator invented: every pointer is read back through
//! Glue's own `GetTable`, every object through Amazon S3's own listing, and every count is
//! compared with the number of records the producer published.
//!
//! The sibling suites pin the neighbouring rules, each of which this one rests on rather than
//! restates in depth: `firehose_iceberg_destination_table_metadata` what the catalog create
//! mints for a table across the create routes and format versions, and
//! `firehose_iceberg_destination_commits_its_kinesis_source` which destination tables a
//! source-fed delivery commits into at all. What is only here is the VERSION HISTORY of a table
//! several flushes have committed to, the SSE member's presence per source family, and the
//! refusal of a catalog target no database or table serves — each a baseline the suites above
//! take for granted.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode};

/// The Region every call below is signed for, and the one the bucket is created in.
const REGION: &str = "us-west-2";

/// How long a commit — or a stream reaching a status — is waited for before a case fails.
///
/// Generous rather than tight: a record has to be consumed from its source before its buffer
/// even opens, and a stream is only usable a clock-owed span after its create, so this is the
/// point at which the delivery is declared never to have happened and not a measurement of how
/// fast the host runs.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(120);

/// How often a status, a listing or a catalog row is looked at again while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The bucket every destination table is located in, and where an error output would go.
const BUCKET: &str = "firehose-iceberg-metadata-versions";

/// The Glue database every destination table lives in.
const DATABASE: &str = "firehose_iceberg_versions_db";

/// The table parameter an Iceberg table's current metadata file is named by, which is what a
/// commit advances, and the parameter marking the row as an open-table-format table. Source:
/// the Data Catalog's own Iceberg table parameters, read back here through `GetTable`.
const METADATA_LOCATION: &str = "metadata_location";
const TABLE_TYPE: &str = "table_type";

/// The open table format the destination tables are, as the `table_type` parameter states it.
const ICEBERG: &str = "ICEBERG";

/// The format version every table of this suite asks its create for (`IcebergInput$Version`),
/// and the number the metadata document states it as.
const FORMAT_VERSION: &str = "2";
const FORMAT_VERSION_NUMBER: i64 = 2;

/// Iceberg's own value for "this table has no current snapshot", which version zero of a
/// table nothing has committed to carries.
const NO_CURRENT_SNAPSHOT: i64 = -1;

/// The trust policy the delivery role carries: Firehose has to be able to assume the role a
/// destination names.
const TRUST_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":"#,
    r#"{"Service":"firehose.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// What the delivery role is allowed: the source read a source-fed stream makes, the catalog
/// read and the catalog advance a commit makes, and the object writes the commit's own objects
/// are.
const ROLE_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"#,
    r#"["kinesis:GetRecords","kinesis:GetShardIterator","kinesis:DescribeStream",""#,
    r#"glue:GetTable","glue:UpdateTable","s3:PutObject"],"Resource":"*"}]}"#
);

/// The columns every destination table declares, as `(column name, the declared Glue type,
/// the Iceberg type the metadata document writes it as)`.
///
/// Source: the Glue column a table declares (`Column$Type` of `StorageDescriptor$Columns`,
/// `smithy_query(query="shape", shape="Column")`) beside Iceberg's primitive vocabulary, which
/// shares most of Hive's names but not all of them — `bigint` is Iceberg's `long`. The types
/// are more than the issue's three on purpose: the schema the document states is the columns
/// as declared, whatever they are.
const COLUMNS: [(&str, &str, &str); 5] = [
    ("id", "string", "string"),
    ("data", "string", "string"),
    ("seq", "bigint", "long"),
    ("score", "double", "double"),
    ("at", "timestamp", "timestamp"),
];

/// One destination-table case: a Firehose stream delivering into an Iceberg table, and the
/// flushes its producer drives.
struct Row {
    case: &'static str,
    /// The destination table's name, which also names its source stream, its delivery stream
    /// and its own location inside [`BUCKET`].
    table: &'static str,
    /// Whether the stream is fed by a Kinesis data stream (`KinesisStreamAsSource`, published
    /// to with `PutRecords`) rather than by direct put (`PutRecordBatch`).
    source_fed: bool,
    /// How many records the producer publishes in each successive flush, one entry per flush.
    flushes: &'static [usize],
}

impl Row {
    /// The Kinesis data stream feeding a source-fed row's delivery stream.
    fn source(&self) -> String {
        format!("{}-source", self.table)
    }

    /// The Firehose stream delivering into this row's destination table.
    fn stream(&self) -> String {
        format!("{}-stream", self.table)
    }

    /// The table's own location: `StorageDescriptor$Location`, where its versions and its data
    /// objects are written.
    fn location(&self) -> String {
        format!("s3://{BUCKET}/{}", self.prefix())
    }

    /// The key prefix of that location inside [`BUCKET`].
    fn prefix(&self) -> String {
        format!("iceberg/{}", self.table)
    }

    /// Where a record this destination could not place would be written.
    fn error_prefix(&self) -> String {
        format!("errors/{}/", self.table)
    }

    /// How many records the producer publishes in all.
    fn records(&self) -> usize {
        self.flushes.iter().sum()
    }
}

/// The rows of the version-history test: the two source families, publishing a different
/// number of records in a different number of successive flushes.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a stream fed by a Kinesis data stream, two flushes",
            table: "kinesis_sourced",
            source_fed: true,
            flushes: &[3, 2],
        },
        Row {
            case: "a direct-put stream, three flushes",
            table: "direct_put",
            source_fed: false,
            flushes: &[1, 4, 2],
        },
    ]
}

/// The five services this flow spans, on ONE shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_kinesis_kinesis_20131202::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_firehose_firehose_20150804::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_glue_awsglue::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`. The signature
/// is never checked: this router runs with authentication off.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One request to the service `signing_name`; answers its status and body bytes.
async fn send(
    router: &MultiServiceRouter,
    signing_name: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> (u16, Vec<u8>) {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization(signing_name));
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(bytes::Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (response.status().as_u16(), response.body().to_vec())
}

/// One `awsJson1_1` operation of `signing_name`, answering its status and parsed body.
async fn json_rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target: &str,
    body: Value,
) -> (u16, Value) {
    let (status, answered) = send(
        router,
        signing_name,
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", target),
        ],
        body.to_string().into_bytes(),
    )
    .await;
    let text = String::from_utf8_lossy(&answered).to_string();
    let parsed = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One operation of an `awsJson1_1` service that must succeed.
async fn call(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: Value,
) -> Value {
    let (status, answered) = json_rpc(
        router,
        signing_name,
        &format!("{target_prefix}.{operation}"),
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: {signing_name} {operation} must succeed, got {status} {answered}"
    );
    answered
}

/// One Kinesis Data Streams operation that must succeed.
async fn kinesis(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    call(router, case, "kinesis", "Kinesis_20131202", operation, body).await
}

/// One Firehose operation, answering its status and body.
async fn firehose_call(
    router: &MultiServiceRouter,
    operation: &str,
    body: Value,
) -> (u16, Value) {
    json_rpc(
        router,
        "firehose",
        &format!("Firehose_20150804.{operation}"),
        body,
    )
    .await
}

/// One Firehose operation that must succeed.
async fn firehose(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    call(
        router,
        case,
        "firehose",
        "Firehose_20150804",
        operation,
        body,
    )
    .await
}

/// One Glue operation that must succeed.
async fn glue(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    call(router, case, "glue", "AWSGlue", operation, body).await
}

/// One `awsQuery` IAM action that must succeed.
async fn iam(router: &MultiServiceRouter, case: &str, action: &str, parameters: &[(&str, &str)]) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (key, value) in parameters {
        body.push('&');
        body.push_str(key);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let (status, answered) = send(
        router,
        "iam",
        "POST",
        "/",
        &[("content-type", "application/x-www-form-urlencoded")],
        body.into_bytes(),
    )
    .await;
    assert_eq!(
        status,
        200,
        "case {case:?}: IAM {action} must succeed, got {status} {}",
        String::from_utf8_lossy(&answered)
    );
}

/// The error code of a refused `awsJson1_1` answer, as its `__type` names it.
fn error_code(answered: &Value) -> String {
    answered["__type"]
        .as_str()
        .unwrap_or_default()
        .rsplit(['#', ':'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The message of a refused `awsJson1_1` answer, whichever casing it carries.
fn error_message(answered: &Value) -> String {
    answered["message"]
        .as_str()
        .or_else(|| answered["Message"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// Create the delivery role, trusted by the Firehose service principal and allowed the reads
/// and writes a commit makes, and answer its ARN.
async fn given_role(router: &MultiServiceRouter, name: &str) -> String {
    iam(
        router,
        "setup",
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", TRUST_POLICY)],
    )
    .await;
    iam(
        router,
        "setup",
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "iceberg-delivery"),
            ("PolicyDocument", ROLE_POLICY),
        ],
    )
    .await;
    format!("arn:aws:iam::{}:role/{name}", engine::emulator_account_id())
}

/// Create the bucket every destination table is located in, and the database they live in.
async fn given_bucket_and_database(router: &MultiServiceRouter) {
    let (status, body) = send(
        router,
        "s3",
        "PUT",
        &format!("/{BUCKET}"),
        &[],
        format!(
            "<CreateBucketConfiguration><LocationConstraint>{REGION}\
             </LocationConstraint></CreateBucketConfiguration>"
        )
        .into_bytes(),
    )
    .await;
    assert_eq!(
        status,
        200,
        "the destination bucket {BUCKET} must be created, got {status} {}",
        String::from_utf8_lossy(&body)
    );
    glue(
        router,
        "setup",
        "CreateDatabase",
        json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;
}

/// Create an Iceberg destination table at `location`, through the `OpenTableFormatInput` route
/// that mints its first metadata file.
async fn given_table(router: &MultiServiceRouter, case: &str, table: &str, location: &str) {
    let declared: Vec<Value> = COLUMNS
        .iter()
        .map(|(name, declared, _)| json!({ "Name": name, "Type": declared }))
        .collect();
    glue(
        router,
        case,
        "CreateTable",
        json!({
            "DatabaseName": DATABASE,
            "TableInput": {
                "Name": table,
                "TableType": "EXTERNAL_TABLE",
                "StorageDescriptor": { "Columns": declared, "Location": location },
            },
            "OpenTableFormatInput": {
                "IcebergInput": { "MetadataOperation": "CREATE", "Version": FORMAT_VERSION },
            },
        }),
    )
    .await;
}

/// One table parameter of the stored catalog row, read back through `GetTable`.
async fn table_parameter(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    parameter: &str,
) -> Option<String> {
    let read = glue(
        router,
        case,
        "GetTable",
        json!({ "DatabaseName": DATABASE, "Name": table }),
    )
    .await;
    read["Table"]["Parameters"][parameter]
        .as_str()
        .map(str::to_string)
}

/// Every key [`BUCKET`] holds under `prefix`, read through `ListObjectsV2`.
async fn keys_under(router: &MultiServiceRouter, case: &str, prefix: &str) -> Vec<String> {
    let (status, body) = send(
        router,
        "s3",
        "GET",
        // Every prefix of this suite is made of unreserved characters and `/`, so it travels
        // in the query string as written.
        &format!("/{BUCKET}?list-type=2&prefix={prefix}"),
        &[],
        Vec::new(),
    )
    .await;
    let text = String::from_utf8_lossy(&body).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: listing {BUCKET}/{prefix} must succeed, got {status} {text}"
    );
    let mut keys = elements(&text, "Key");
    keys.sort();
    keys
}

/// The bytes [`BUCKET`] holds at `key`.
async fn object(router: &MultiServiceRouter, case: &str, key: &str) -> Vec<u8> {
    let (status, body) = send(
        router,
        "s3",
        "GET",
        &format!("/{BUCKET}/{key}"),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(
        status,
        200,
        "case {case:?}: reading {BUCKET}/{key} must succeed, got {status} {}",
        String::from_utf8_lossy(&body)
    );
    body
}

/// The document stored at the `s3://` location `pointer` — the read an Iceberg client makes of
/// a table's current metadata file.
async fn document_at(router: &MultiServiceRouter, case: &str, pointer: &str) -> Value {
    let key = pointer
        .strip_prefix(&format!("s3://{BUCKET}/"))
        .unwrap_or_else(|| {
            panic!("case {case:?}: the pointer {pointer} must name an object of {BUCKET}")
        });
    let body = object(router, case, key).await;
    serde_json::from_slice(&body).unwrap_or_else(|error| {
        panic!("case {case:?}: the metadata file {pointer} must be JSON, got {error}")
    })
}

/// The VERSION ORDINAL the name of a metadata file carries: Iceberg numbers a table's metadata
/// files from zero, and the name is `NNNNN-<id>.metadata.json`.
fn ordinal_of(case: &str, name: &str) -> u64 {
    let file = name.rsplit('/').next().unwrap_or(name);
    let digits = file.split('-').next().unwrap_or_default();
    digits.parse().unwrap_or_else(|_| {
        panic!(
            "case {case:?}: the metadata file {name} must be numbered NNNNN-<id>.metadata.json"
        )
    })
}

/// The snapshots a metadata document holds, in the order it holds them.
fn snapshots(document: &Value) -> Vec<Value> {
    document["snapshots"].as_array().cloned().unwrap_or_default()
}

/// The snapshot a metadata document reports as current.
fn current_snapshot(document: &Value) -> Option<Value> {
    let current = document["current-snapshot-id"].clone();
    snapshots(document)
        .into_iter()
        .find(|snapshot| snapshot["snapshot-id"] == current)
}

/// One snapshot summary total, as the document carries it (Iceberg states a summary's values
/// as strings).
///
/// A summary that states no such total at all fails the case for THAT reason rather than as a
/// count that did not match: a snapshot whose summary omits what it added is not a snapshot a
/// reader can size the table by.
fn total(case: &str, snapshot: &Value, name: &str) -> i64 {
    snapshot["summary"][name]
        .as_str()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or_else(|| {
            panic!("case {case:?}: the snapshot summary must state {name}, got {snapshot}")
        })
}

/// The records the row's producer publishes in one flush: one JSON object per record, each
/// value of the type the destination table's column declares.
fn payloads(row: &Row, already_published: usize, records: usize) -> Vec<Vec<u8>> {
    (already_published..already_published + records)
        .map(|index| {
            json!({
                "id": format!("{}-{index}", row.table),
                "data": format!("payload {index}"),
                "seq": index as i64,
                "score": index as f64 / 4.0,
                "at": "2026-01-02T03:04:05Z",
            })
            .to_string()
            .into_bytes()
        })
        .collect()
}

/// Publish one flush's records: to the row's Kinesis source with `PutRecords` for a source-fed
/// row, and to the stream itself with `PutRecordBatch` for a direct-put one.
async fn publish(router: &MultiServiceRouter, row: &Row, records: &[Vec<u8>]) {
    if row.source_fed {
        let entries: Vec<Value> = records
            .iter()
            .enumerate()
            .map(|(index, payload)| {
                json!({
                    "PartitionKey": format!("pk-{index}"),
                    "Data": ::aws_smithy_types::base64::encode(payload),
                })
            })
            .collect();
        let answered = kinesis(
            router,
            row.case,
            "PutRecords",
            json!({ "StreamName": row.source(), "Records": entries }),
        )
        .await;
        assert_eq!(
            answered["FailedRecordCount"].as_i64().unwrap_or(-1),
            0,
            "case {:?}: the source takes every record of the batch, got {answered}",
            row.case
        );
        return;
    }
    let entries: Vec<Value> = records
        .iter()
        .map(|payload| json!({ "Data": ::aws_smithy_types::base64::encode(payload) }))
        .collect();
    let answered = firehose(
        router,
        row.case,
        "PutRecordBatch",
        json!({ "DeliveryStreamName": row.stream(), "Records": entries }),
    )
    .await;
    assert_eq!(
        answered["FailedPutCount"].as_i64().unwrap_or(-1),
        0,
        "case {:?}: the stream takes every record of the batch, got {answered}",
        row.case
    );
}

/// Create the row's source Kinesis data stream and answer the `StreamARN` it reports once it
/// is `ACTIVE`.
///
/// A poll loop on the STATUS rather than a wait for the clock-owed span, so how fast the host
/// runs cannot decide a case.
async fn given_source(router: &MultiServiceRouter, row: &Row) -> String {
    let source = row.source();
    kinesis(
        router,
        row.case,
        "CreateStream",
        json!({ "StreamName": source, "ShardCount": 1 }),
    )
    .await;
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let described = kinesis(
            router,
            row.case,
            "DescribeStreamSummary",
            json!({ "StreamName": source }),
        )
        .await;
        let summary = &described["StreamDescriptionSummary"];
        if summary["StreamStatus"] == json!("ACTIVE") {
            return summary["StreamARN"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!(
                        "case {:?}: the source {source} reports its ARN, got {described}",
                        row.case
                    )
                })
                .to_string();
        }
        assert!(
            Instant::now() < deadline,
            "case {:?}: the source {source} must reach ACTIVE, got {described}",
            row.case
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `IcebergDestinationConfiguration` of a stream delivering into `table`, with the
/// buffering hints that make a flush due as soon as the records are there.
fn iceberg_destination(
    role_arn: &str,
    database: &str,
    table: &str,
    error_prefix: &str,
) -> Value {
    json!({
        "RoleARN": role_arn,
        "CatalogConfiguration": {
            "CatalogARN": format!(
                "arn:aws:glue:{REGION}:{}:catalog",
                engine::emulator_account_id()
            ),
        },
        "DestinationTableConfigurationList": [{
            "DestinationDatabaseName": database,
            "DestinationTableName": table,
        }],
        "S3Configuration": {
            "RoleARN": role_arn,
            "BucketARN": format!("arn:aws:s3:::{BUCKET}"),
            "ErrorOutputPrefix": error_prefix,
        },
        // The smallest size and the shortest interval the member admits, so a case waits for
        // the commit and not for a buffer window it did not ask to test.
        "BufferingHints": { "SizeInMBs": 1, "IntervalInSeconds": 0 },
    })
}

/// Create the row's Firehose stream: fed by `source_arn` when the row is source-fed, and a
/// direct-put stream otherwise.
async fn create_stream(
    router: &MultiServiceRouter,
    row: &Row,
    source_arn: Option<&str>,
    role_arn: &str,
) {
    let stream = row.stream();
    let mut request = json!({
        "DeliveryStreamName": stream,
        "DeliveryStreamType": if row.source_fed { "KinesisStreamAsSource" } else { "DirectPut" },
        "IcebergDestinationConfiguration":
            iceberg_destination(role_arn, DATABASE, row.table, &row.error_prefix()),
    });
    if let Some(source_arn) = source_arn {
        request["KinesisStreamSourceConfiguration"] = json!({
            "KinesisStreamARN": source_arn,
            "RoleARN": role_arn,
        });
    }
    let answered = firehose(router, row.case, "CreateDeliveryStream", request).await;
    assert!(
        answered["DeliveryStreamARN"]
            .as_str()
            .is_some_and(|arn| arn.ends_with(&format!("deliverystream/{stream}"))),
        "case {:?}: the create answers {stream}'s ARN, got {answered}",
        row.case
    );
}

/// Wait for a stream to report `ACTIVE`, answering its description.
async fn await_active(router: &MultiServiceRouter, case: &str, stream: &str) -> Value {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let described = firehose(
            router,
            case,
            "DescribeDeliveryStream",
            json!({ "DeliveryStreamName": stream }),
        )
        .await;
        if described["DeliveryStreamDescription"]["DeliveryStreamStatus"] == json!("ACTIVE") {
            return described["DeliveryStreamDescription"].clone();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {stream} must reach ACTIVE, got {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The row's advanced catalog pointer and the metadata document it names, once the table's
/// current snapshot accounts for every record the producer has published — or a failure once
/// [`DELIVERY_DEADLINE`] has passed.
///
/// The POINTER is what is polled because the catalog is advanced only after the objects of a
/// commit are written, so a pointer that moved is a snapshot whose objects are all there; and
/// the TOTAL is what is waited on because a flush may be committed in more than one piece, each
/// of which appends a snapshot of its own.
async fn committed(
    router: &MultiServiceRouter,
    row: &Row,
    before: &str,
    published: usize,
) -> (String, Value) {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let pointer = table_parameter(router, row.case, row.table, METADATA_LOCATION)
            .await
            .unwrap_or_default();
        if pointer != before && !pointer.is_empty() {
            let document = document_at(router, row.case, &pointer).await;
            if let Some(snapshot) = current_snapshot(&document) {
                let records = i64::try_from(published).unwrap_or(i64::MAX);
                if total(row.case, &snapshot, "total-records") == records {
                    return (pointer, document);
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "case {:?}: expected the destination table {} to hold a committed snapshot \
             accounting for all {published} published record(s), its catalog pointer is \
             {pointer:?} (it was {before:?})",
            row.case,
            row.table
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Assert that `document` is a version of the table at `location` declaring the suite's
/// columns: what every version of the table states, whichever commit wrote it.
fn assert_table_identity(case: &str, document: &Value, location: &str, uuid: &Value) {
    assert_eq!(
        document["format-version"].as_i64(),
        Some(FORMAT_VERSION_NUMBER),
        "case {case:?}: the version the create asked for is what every version states, got \
         {document}"
    );
    assert_eq!(
        document["location"].as_str(),
        Some(location.trim_end_matches('/')),
        "case {case:?}: every version states the table's own location, got {document}"
    );
    assert_eq!(
        &document["table-uuid"], uuid,
        "case {case:?}: every version is a version of the SAME table, got {document}"
    );
    let fields = document["schemas"][0]["fields"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        fields.len(),
        COLUMNS.len(),
        "case {case:?}: the schema holds one field per declared column, got {document}"
    );
    for (index, (name, _declared, iceberg)) in COLUMNS.iter().enumerate() {
        assert_eq!(
            fields[index],
            json!({ "id": index + 1, "name": name, "required": false, "type": iceberg }),
            "case {case:?}: field {} must be the column {name} typed {iceberg}, got {}",
            index + 1,
            fields[index]
        );
    }
}

/// RULE: the create that marks a table as Iceberg writes version ZERO of its metadata document
/// — the format version asked for, the table's own location, the declared columns as Iceberg
/// fields, and no snapshot — at the location the catalog row then points at.
#[tokio::test]
async fn an_iceberg_tables_create_writes_version_zero_of_its_metadata() {
    let router = emulator();
    given_bucket_and_database(&router).await;

    // The rows differ along the shape of the table's location, which is what the version's
    // name is built under: a trailing slash or none, one segment or several.
    let cases: [(&str, &str, String); 3] = [
        (
            "a location with a trailing slash",
            "slashed",
            format!("s3://{BUCKET}/iceberg/slashed/"),
        ),
        (
            "a location without one",
            "unslashed",
            format!("s3://{BUCKET}/iceberg/unslashed"),
        ),
        (
            "a location several segments deep",
            "nested",
            format!("s3://{BUCKET}/lake/warehouse/iceberg/nested/"),
        ),
    ];

    for (case, table, location) in &cases {
        given_table(&router, case, table, location).await;
        assert_eq!(
            table_parameter(&router, case, table, TABLE_TYPE).await,
            Some(ICEBERG.to_string()),
            "case {case:?}: the create marks the row as an open-table-format table"
        );
        let pointer = table_parameter(&router, case, table, METADATA_LOCATION)
            .await
            .unwrap_or_else(|| {
                panic!("case {case:?}: the create records where the table's metadata lives")
            });
        assert!(
            pointer.starts_with(location.trim_end_matches('/'))
                && pointer.ends_with(".metadata.json"),
            "case {case:?}: the pointer names a metadata file under the table's own location, \
             got {pointer}"
        );
        assert_eq!(
            ordinal_of(case, &pointer),
            0,
            "case {case:?}: the create's own version is the table's version zero, got {pointer}"
        );

        // The object the pointer names is THERE, and it is the table: a reader that resolves
        // the pointer gets the document, not a missing key.
        let document = document_at(&router, case, &pointer).await;
        let uuid = document["table-uuid"].clone();
        assert!(
            uuid.as_str().is_some_and(|uuid| !uuid.is_empty()),
            "case {case:?}: the document states the table's identity, got {document}"
        );
        assert_table_identity(case, &document, location, &uuid);
        assert_eq!(
            snapshots(&document),
            Vec::<Value>::new(),
            "case {case:?}: a table nothing has committed to holds no snapshot, got {document}"
        );
        assert_eq!(
            document["current-snapshot-id"].as_i64(),
            Some(NO_CURRENT_SNAPSHOT),
            "case {case:?}: and so reports no current snapshot either, got {document}"
        );

        // Exactly one version is there: the one the create wrote.
        let versions: Vec<String> = keys_under(&router, case, &metadata_prefix(location))
            .await
            .into_iter()
            .filter(|key| key.ends_with(".metadata.json"))
            .collect();
        assert_eq!(
            versions.len(),
            1,
            "case {case:?}: the create writes exactly one version, got {versions:?}"
        );
    }
}

/// The key prefix a table's metadata files live under, for a location with or without its
/// trailing slash.
fn metadata_prefix(location: &str) -> String {
    let path = location
        .trim_start_matches("s3://")
        .trim_start_matches(BUCKET)
        .trim_start_matches('/')
        .trim_end_matches('/');
    format!("{path}/metadata/")
}

/// RULE: every flush an Iceberg destination commits appends one snapshot by writing the NEXT
/// numbered version of the destination table's metadata beside the versions already there, and
/// moves the catalog's `metadata_location` to it — so a version's ordinal is the number of
/// snapshots it holds, the current version is the newest ordinal present, each earlier version
/// is still readable under the same table identity, and the current document's snapshots
/// account for exactly the records the producer published.
#[tokio::test]
async fn each_flush_commits_the_next_metadata_version_and_keeps_the_earlier_ones() {
    let router = emulator();
    given_bucket_and_database(&router).await;
    let role_arn = given_role(&router, "firehose-iceberg-versions-role").await;
    let rows = rows();

    // The destination tables, and the version each one starts from.
    let mut history: Vec<Vec<String>> = Vec::new();
    let mut identities: Vec<Value> = Vec::new();
    for row in &rows {
        given_table(&router, row.case, row.table, &row.location()).await;
        let pointer = table_parameter(&router, row.case, row.table, METADATA_LOCATION)
            .await
            .unwrap_or_else(|| panic!("case {:?}: the create records a pointer", row.case));
        let document = document_at(&router, row.case, &pointer).await;
        identities.push(document["table-uuid"].clone());
        history.push(vec![pointer]);
    }

    // The sources, then the streams: every create is made before any status is waited for, so
    // the clock-owed spans of the whole suite overlap.
    let mut sources: Vec<Option<String>> = Vec::new();
    for row in &rows {
        sources.push(if row.source_fed {
            Some(given_source(&router, row).await)
        } else {
            None
        });
    }
    for (row, source_arn) in rows.iter().zip(&sources) {
        create_stream(&router, row, source_arn.as_deref(), &role_arn).await;
    }
    for row in &rows {
        await_active(&router, row.case, &row.stream()).await;
    }

    // THE FLUSHES, one round at a time: every row publishes its round, and every row's commit
    // is then waited for, so the rounds of the rows overlap and a row's own flushes stay in
    // order (a flush is published only once the one before it has committed).
    let longest = rows.iter().map(|row| row.flushes.len()).max().unwrap_or(0);
    let mut published: Vec<usize> = vec![0; rows.len()];
    for round in 0..longest {
        let mut publishing: Vec<usize> = Vec::new();
        for (index, row) in rows.iter().enumerate() {
            let Some(records) = row.flushes.get(round) else {
                continue;
            };
            let batch = payloads(row, published[index], *records);
            publish(&router, row, &batch).await;
            published[index] += records;
            publishing.push(index);
        }
        for index in publishing {
            let row = &rows[index];
            let before = history[index]
                .last()
                .cloned()
                .unwrap_or_default();
            let (pointer, document) = committed(&router, row, &before, published[index]).await;
            assert!(
                ordinal_of(row.case, &pointer) > ordinal_of(row.case, &before),
                "case {:?}: flush {} commits a NEWER version than {before}, got {pointer}",
                row.case,
                round + 1
            );
            assert_eq!(
                ordinal_of(row.case, &pointer),
                u64::try_from(snapshots(&document).len()).unwrap_or(u64::MAX),
                "case {:?}: a version's ordinal is the number of snapshots it holds, got \
                 {pointer} holding {}",
                row.case,
                snapshots(&document).len()
            );
            history[index].push(pointer);
        }
    }

    // WHAT THE TABLE THEN IS: the newest version is the one the catalog points at, every
    // version ever written is still there and still readable, and the snapshots account for
    // every published record exactly once.
    for ((row, versions), uuid) in rows.iter().zip(&history).zip(&identities) {
        let case = row.case;
        let current = versions.last().cloned().unwrap_or_default();
        assert_eq!(
            table_parameter(&router, case, row.table, METADATA_LOCATION).await,
            Some(current.clone()),
            "case {case:?}: the catalog points at the version the last commit wrote"
        );
        let document = document_at(&router, case, &current).await;
        let committed_snapshots = snapshots(&document);
        assert!(
            committed_snapshots.len() >= row.flushes.len(),
            "case {case:?}: each of the {} flush(es) appended at least one snapshot, got {}",
            row.flushes.len(),
            committed_snapshots.len()
        );
        assert_table_identity(case, &document, &row.location(), uuid);

        // Every record published, counted once: the summaries of the snapshots add up to it,
        // and the current snapshot's running total states it.
        let added: i64 = committed_snapshots
            .iter()
            .map(|snapshot| total(case, snapshot, "added-records"))
            .sum();
        let expected = i64::try_from(row.records()).unwrap_or(i64::MAX);
        assert_eq!(
            added, expected,
            "case {case:?}: the snapshots added the {expected} published record(s), got \
             {document}"
        );
        let snapshot = current_snapshot(&document).unwrap_or_else(|| {
            panic!("case {case:?}: the document must hold its current snapshot, got {document}")
        });
        assert_eq!(
            total(case, &snapshot, "total-records"),
            expected,
            "case {case:?}: the current snapshot's running total is every record the table \
             holds, got {snapshot}"
        );
        assert_eq!(
            total(case, &snapshot, "total-data-files"),
            i64::try_from(committed_snapshots.len()).unwrap_or(i64::MAX),
            "case {case:?}: each commit contributed its own data file, got {snapshot}"
        );

        // The current snapshot is the one a reader resolves through the document's history and
        // its main branch.
        assert_eq!(
            committed_snapshots
                .last()
                .map(|snapshot| snapshot["snapshot-id"].clone()),
            Some(document["current-snapshot-id"].clone()),
            "case {case:?}: the snapshot the last commit appended is the current one, got \
             {document}"
        );
        let log = document["snapshot-log"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            log.len(),
            committed_snapshots.len(),
            "case {case:?}: the history holds one entry per snapshot, got {document}"
        );
        assert_eq!(
            log.last().map(|entry| entry["snapshot-id"].clone()),
            Some(document["current-snapshot-id"].clone()),
            "case {case:?}: its last entry is the current snapshot, got {document}"
        );
        assert_eq!(
            document["refs"]["main"]["snapshot-id"],
            document["current-snapshot-id"],
            "case {case:?}: the main branch names the current snapshot, got {document}"
        );

        // EVERY version is still where it was written, and each one holds exactly the history
        // it was written with: the ordinal is the snapshot count, so no commit overwrote a
        // version or dropped a snapshot an earlier version held.
        let keys = keys_under(&router, case, &format!("{}/metadata/", row.prefix())).await;
        let versions_present: Vec<String> = keys
            .iter()
            .filter(|key| key.ends_with(".metadata.json"))
            .cloned()
            .collect();
        assert_eq!(
            versions_present.len(),
            committed_snapshots.len() + 1,
            "case {case:?}: the create's version and one per commit are all there, got \
             {versions_present:?}"
        );
        for version in versions {
            let document = document_at(&router, case, version).await;
            assert_eq!(
                ordinal_of(case, version),
                u64::try_from(snapshots(&document).len()).unwrap_or(u64::MAX),
                "case {case:?}: the version {version} still holds the {} snapshot(s) its \
                 ordinal states",
                ordinal_of(case, version)
            );
            assert_table_identity(case, &document, &row.location(), uuid);
        }

        // One data object per commit, each a Parquet file, and nothing under the error output:
        // every record the producer published was placed in the table. The count is anchored in
        // the producer's own flushes as well, so the case does not rest on the table's
        // bookkeeping agreeing with itself.
        let data = keys_under(&router, case, &format!("{}/data/", row.prefix())).await;
        assert!(
            data.len() >= row.flushes.len(),
            "case {case:?}: each of the {} flush(es) wrote a data object, got {data:?}",
            row.flushes.len()
        );
        assert_eq!(
            data.len(),
            committed_snapshots.len(),
            "case {case:?}: each commit wrote its own data object, got {data:?}"
        );
        for key in &data {
            assert!(
                key.ends_with(".parquet"),
                "case {case:?}: the committed data objects are Parquet files, got {data:?}"
            );
            let body = object(&router, case, key).await;
            assert!(
                body.starts_with(b"PAR1"),
                "case {case:?}: the data object {key} opens with the Parquet magic, got {:?}",
                body.get(..4)
            );
        }
        let errors = keys_under(&router, case, &row.error_prefix()).await;
        assert!(
            errors.is_empty(),
            "case {case:?}: every published record was committed, so the error output stays \
             empty, got {errors:?}"
        );
    }
}

/// RULE: server-side encryption applies to the direct-put path alone, so the description of an
/// Iceberg-destination stream carries `DeliveryStreamEncryptionConfiguration` when the stream
/// is `DirectPut` and leaves the member out for a stream fed by a configured source — before
/// and after a sibling direct-put stream has SSE turned on.
#[tokio::test]
async fn only_a_direct_put_streams_description_carries_its_sse_state() {
    let router = emulator();
    given_bucket_and_database(&router).await;
    let role_arn = given_role(&router, "firehose-iceberg-sse-role").await;

    // The rows differ along what feeds the stream: direct put, which SSE applies to, and the
    // source-fed type whose source this emulator takes.
    let rows = vec![
        Row {
            case: "a direct-put Iceberg stream",
            table: "sse_direct_put",
            source_fed: false,
            flushes: &[],
        },
        Row {
            case: "an Iceberg stream fed by a Kinesis data stream",
            table: "sse_kinesis_sourced",
            source_fed: true,
            flushes: &[],
        },
    ];

    for row in &rows {
        given_table(&router, row.case, row.table, &row.location()).await;
        let source_arn = if row.source_fed {
            Some(given_source(&router, row).await)
        } else {
            None
        };
        create_stream(&router, row, source_arn.as_deref(), &role_arn).await;
    }

    for row in &rows {
        let description = await_active(&router, row.case, &row.stream()).await;
        let reported = &description["DeliveryStreamEncryptionConfiguration"];
        assert_eq!(
            reported.is_null(),
            row.source_fed,
            "case {:?}: only a direct-put stream has an SSE state to report, got {description}",
            row.case
        );
        if !row.source_fed {
            assert_eq!(
                reported["Status"].as_str(),
                Some("DISABLED"),
                "case {:?}: a stream nothing configured SSE on reports it disabled, got \
                 {description}",
                row.case
            );
        }
    }

    // SSE turned on where it applies changes nothing about the stream it does not: the
    // source-fed stream's description still carries no such member.
    let direct_put = rows
        .iter()
        .find(|row| !row.source_fed)
        .expect("one direct-put row");
    firehose(
        &router,
        direct_put.case,
        "StartDeliveryStreamEncryption",
        json!({
            "DeliveryStreamName": direct_put.stream(),
            "DeliveryStreamEncryptionConfigurationInput": { "KeyType": "AWS_OWNED_CMK" },
        }),
    )
    .await;
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let described = firehose(
            &router,
            direct_put.case,
            "DescribeDeliveryStream",
            json!({ "DeliveryStreamName": direct_put.stream() }),
        )
        .await;
        let reported = &described["DeliveryStreamDescription"]
            ["DeliveryStreamEncryptionConfiguration"]["Status"];
        if reported == &json!("ENABLED") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {:?}: SSE must reach ENABLED on the stream it applies to, got {described}",
            direct_put.case
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    for row in rows.iter().filter(|row| row.source_fed) {
        let described = firehose(
            &router,
            row.case,
            "DescribeDeliveryStream",
            json!({ "DeliveryStreamName": row.stream() }),
        )
        .await;
        assert!(
            described["DeliveryStreamDescription"]["DeliveryStreamEncryptionConfiguration"]
                .is_null(),
            "case {:?}: a source-fed stream reports no SSE state whatever its siblings do, got \
             {described}",
            row.case
        );
    }
}

/// RULE: the Data Catalog target of an Iceberg destination is resolved by the call that submits
/// it, so a create naming a table the catalog cannot serve — whichever half of the target is
/// missing — is refused with `InvalidArgumentException` stating the catalog read it made, and
/// registers no stream.
#[tokio::test]
async fn a_create_naming_an_unservable_catalog_target_is_refused_and_registers_nothing() {
    let router = emulator();
    given_bucket_and_database(&router).await;
    let role_arn = given_role(&router, "firehose-iceberg-target-role").await;
    let present = "target_present";
    given_table(
        &router,
        "setup",
        present,
        &format!("s3://{BUCKET}/iceberg/{present}/"),
    )
    .await;

    // (case, the database the destination names, the table it names, whether the catalog can
    // serve that target)
    let cases: [(&str, &str, &str, bool); 4] = [
        ("the table the catalog holds", DATABASE, present, true),
        (
            "a table the database does not hold",
            DATABASE,
            "no_such_table",
            false,
        ),
        (
            "a database the catalog does not hold",
            "no_such_database",
            present,
            false,
        ),
        (
            "neither the database nor the table",
            "no_such_database",
            "no_such_table",
            false,
        ),
    ];

    for (ordinal, (case, database, table, servable)) in cases.iter().enumerate() {
        let stream = format!("target-{ordinal}");
        let (status, answered) = firehose_call(
            &router,
            "CreateDeliveryStream",
            json!({
                "DeliveryStreamName": stream,
                "DeliveryStreamType": "DirectPut",
                "IcebergDestinationConfiguration":
                    iceberg_destination(&role_arn, database, table, "errors/target/"),
            }),
        )
        .await;
        if *servable {
            assert_eq!(
                status, 200,
                "case {case:?}: the catalog serves this target, got {status} {answered}"
            );
            let listed = firehose(&router, case, "ListDeliveryStreams", json!({})).await;
            assert!(
                listed["DeliveryStreamNames"]
                    .as_array()
                    .unwrap_or(&Vec::new())
                    .iter()
                    .any(|name| name == &json!(stream)),
                "case {case:?}: the created stream is one of this account's, got {listed}"
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {case:?}: the call is refused as a client error, got {status} {answered}"
        );
        assert_eq!(
            error_code(&answered),
            "InvalidArgumentException",
            "case {case:?}: expected InvalidArgumentException, got {answered}"
        );
        assert_eq!(
            error_message(&answered),
            format!(
                "Role {role_arn} is not authorized to perform: glue:GetTable for the given \
                 table or the table does not exist."
            ),
            "case {case:?}: expected the catalog-read sentence naming the request's role, got \
             {answered}"
        );
        // A refused create leaves nothing behind: the name it asked for is still free.
        let listed = firehose(&router, case, "ListDeliveryStreams", json!({})).await;
        assert!(
            !listed["DeliveryStreamNames"]
                .as_array()
                .unwrap_or(&Vec::new())
                .iter()
                .any(|name| name == &json!(stream)),
            "case {case:?}: a refused create registers no stream, got {listed}"
        );
        let (status, described) = firehose_call(
            &router,
            "DescribeDeliveryStream",
            json!({ "DeliveryStreamName": stream }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: no stream of that name exists, got {status} {described}"
        );
        assert_eq!(
            error_code(&described),
            "ResourceNotFoundException",
            "case {case:?}: expected the not-found refusal, got {described}"
        );
    }
}
