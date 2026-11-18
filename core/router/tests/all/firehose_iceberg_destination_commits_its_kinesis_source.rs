// SPDX-License-Identifier: CC0-1.0

//! A Firehose stream FED BY A KINESIS DATA STREAM whose destination is an Apache Iceberg
//! table of the Data Catalog: what each flush commits into that table, and what a
//! destination table the catalog holds no Iceberg metadata for gets.
//!
//! RULE: a Firehose stream delivers every buffer it flushes to the destination its
//! configuration names, whatever feeds the stream — so an `IcebergDestinationConfiguration`
//! ("Configure Apache Iceberg Tables destination") naming a `CatalogConfiguration` and a
//! `DestinationTableConfigurationList` commits each buffer into the destination table the
//! catalog holds Iceberg metadata for: a data object under that table's own location, the
//! manifest and manifest list its snapshot records those data files in, a new metadata file
//! whose current snapshot accounts for every record committed, and the catalog row's
//! `metadata_location` moved to that file. A destination table the catalog holds no Iceberg
//! metadata for — a row with no `table_type`/`metadata_location` marker, or one labelled
//! `ICEBERG` with no metadata file to commit onto — has no snapshot history to append to, so
//! the stream accepts its records and writes NOTHING for it, and never retro-fits a metadata
//! location onto the row.
//!
//! Which side of that boundary a destination table falls on is decided by the MARKER the
//! catalog row carries and by nothing else: how the row came to carry it does not matter, so
//! metadata a `CreateTable` minted (`OpenTableFormatInput`) and the same two parameters
//! written by a client both commit.
//!
//! Only a cross-service suite can pin this on the path a client drives: the flow spans five
//! services mounted on ONE engine exactly as `core/router/src/lib.rs` mounts them — Kinesis
//! Data Streams for the source each stream consumes, IAM for the delivery role Firehose
//! resolves and the object write is authorized under, Glue for the Data Catalog the
//! destination tables live in, Firehose for the streams, and Amazon S3 for the objects a
//! commit writes and the metadata file the catalog then points at.
//!
//! The rows differ along what the rule generalises over: whether the destination table
//! carries the Iceberg marker at all, HOW it came to carry it (minted by the create, or
//! written by the client as table parameters), the marker that is only a label (a
//! `table_type` with no metadata file), how many records the producer publishes, and whether
//! it publishes them with one `PutRecords` or one `PutRecord` per record.
//!
//! Nothing here asserts a value this emulator invented: every pointer is read back through
//! Glue's own `GetTable`, every object through Amazon S3's own listing, and the counts are
//! compared with the number of records the producer published.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode};

/// The Region every call below is signed for, and the one the bucket is created in.
const REGION: &str = "us-west-2";

/// How long a commit — or a source-fed delivery — is waited for before a case fails.
///
/// Generous rather than tight: a record has to be consumed from its source before its buffer
/// even opens, and the stream itself is only usable a clock-owed span after its create, so
/// this is the point at which the delivery is declared never to have happened and not a
/// measurement of how fast the host runs.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(120);

/// How often a status, a listing or a catalog row is looked at again while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long a destination table that must take NOTHING is watched, by looking again every
/// [`POLL_INTERVAL`], once the marked tables have demonstrably committed: several source polls
/// and buffer windows, so a late write fails the case whenever in the window it happens.
const WATCH: Duration = Duration::from_millis(1_500);

/// The bucket every destination table is located in, and where the error output goes.
const BUCKET: &str = "firehose-iceberg-source-fed";

/// The Glue database every destination table lives in.
const DATABASE: &str = "firehose_iceberg_source_fed_db";

/// The table parameter an Iceberg table's current metadata file is named by, which is what a
/// commit advances. Source: the Data Catalog's own Iceberg table parameters, read back here
/// through `GetTable`.
const METADATA_LOCATION: &str = "metadata_location";

/// The parameter marking a catalog row as an open-table-format table, and the value an Iceberg
/// table carries in it.
const TABLE_TYPE: &str = "table_type";
const ICEBERG: &str = "ICEBERG";

/// The trust policy the delivery role carries: Firehose has to be able to assume the role a
/// destination names.
const TRUST_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":"#,
    r#"{"Service":"firehose.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// What the delivery role is allowed: the source read a `KinesisStreamAsSource` stream makes,
/// the catalog reads and the table update an Iceberg commit makes, and the object write that
/// commit's own objects are.
const ROLE_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"#,
    r#"["kinesis:GetRecords","kinesis:GetShardIterator","kinesis:DescribeStream",""#,
    r#"glue:GetTable","glue:UpdateTable","s3:PutObject"],"Resource":"*"}]}"#
);

/// How a destination table's catalog row came to carry — or not carry — the Iceberg marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker {
    /// `CreateTable` with an `OpenTableFormatInput`, so the catalog minted the table's first
    /// metadata file and recorded it on the row.
    Minted,
    /// A client that writes the two table parameters itself, naming a metadata file of its own
    /// under the table's location.
    ClientWritten,
    /// A Hive-style row: no Iceberg parameter at all.
    Absent,
    /// A row LABELLED `ICEBERG` with no metadata file for a commit to append onto.
    LabelOnly,
}

/// One destination-table case.
struct Row {
    case: &'static str,
    /// The destination table's name, which also names its source stream, its delivery stream
    /// and its own location inside [`BUCKET`].
    table: &'static str,
    marker: Marker,
    /// Whether a delivery into this table commits — which the marker alone decides.
    commits: bool,
    /// How many records the producer publishes to this table's source.
    records: usize,
    /// Whether they are published with ONE `PutRecords` (true) or one `PutRecord` each.
    batch: bool,
}

impl Row {
    /// The Kinesis data stream feeding this row's delivery stream.
    fn source(&self) -> String {
        format!("{}-source", self.table)
    }

    /// The Firehose stream delivering this row's source into its destination table.
    fn stream(&self) -> String {
        format!("{}-stream", self.table)
    }

    /// The table's own location: `StorageDescriptor$Location`, where a commit writes.
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

    /// The parameters this row's `CreateTable` writes onto the catalog row.
    fn parameters(&self) -> Option<Value> {
        match self.marker {
            // Minted by the create itself, so the create writes none.
            Marker::Minted | Marker::Absent => None,
            Marker::ClientWritten => Some(json!({
                TABLE_TYPE: ICEBERG,
                METADATA_LOCATION:
                    format!("{}/metadata/00000-{}.metadata.json", self.location(), self.table),
            })),
            Marker::LabelOnly => Some(json!({ TABLE_TYPE: ICEBERG })),
        }
    }
}

/// The columns every destination table declares, as `(name, declared Glue type)`.
const COLUMNS: [(&str, &str); 2] = [("id", "string"), ("seq", "bigint")];

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a destination table whose Iceberg metadata the create minted",
            table: "minted_events",
            marker: Marker::Minted,
            commits: true,
            records: 3,
            batch: true,
        },
        Row {
            case: "a destination table a client marked with the Iceberg table parameters",
            table: "client_marked_events",
            marker: Marker::ClientWritten,
            commits: true,
            records: 2,
            batch: false,
        },
        Row {
            case: "a destination table carrying no Iceberg marker at all",
            table: "unmarked_events",
            marker: Marker::Absent,
            commits: false,
            records: 3,
            batch: true,
        },
        Row {
            case: "a destination table labelled ICEBERG with no metadata file",
            table: "labelled_events",
            marker: Marker::LabelOnly,
            commits: false,
            records: 2,
            batch: false,
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

/// One Firehose operation that must succeed.
async fn firehose(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    call(router, case, "firehose", "Firehose_20150804", operation, body).await
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

/// Create the delivery role, trusted by the Firehose service principal and allowed the source
/// read, the catalog advance and the object write a commit makes, and answer its ARN.
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

/// Create the bucket every destination table is located in.
async fn given_bucket(router: &MultiServiceRouter) {
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
}

/// Create the row's destination table in the Data Catalog, marked the way the row names.
async fn given_table(router: &MultiServiceRouter, row: &Row) {
    let declared: Vec<Value> = COLUMNS
        .iter()
        .map(|(name, declared)| json!({ "Name": name, "Type": declared }))
        .collect();
    let mut input = json!({
        "Name": row.table,
        "TableType": "EXTERNAL_TABLE",
        "StorageDescriptor": { "Columns": declared, "Location": row.location() },
    });
    if let Some(parameters) = row.parameters() {
        input["Parameters"] = parameters;
    }
    let mut request = json!({ "DatabaseName": DATABASE, "TableInput": input });
    if row.marker == Marker::Minted {
        request["OpenTableFormatInput"] =
            json!({ "IcebergInput": { "MetadataOperation": "CREATE" } });
    }
    glue(router, row.case, "CreateTable", request).await;
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

/// Create the row's `KinesisStreamAsSource` Firehose stream, delivering into the row's
/// destination table.
async fn create_stream(
    router: &MultiServiceRouter,
    row: &Row,
    source_arn: &str,
    role_arn: &str,
) {
    let stream = row.stream();
    let answered = firehose(
        router,
        row.case,
        "CreateDeliveryStream",
        json!({
            "DeliveryStreamName": stream,
            "DeliveryStreamType": "KinesisStreamAsSource",
            "KinesisStreamSourceConfiguration": {
                "KinesisStreamARN": source_arn,
                "RoleARN": role_arn,
            },
            "IcebergDestinationConfiguration": {
                "RoleARN": role_arn,
                "CatalogConfiguration": {
                    "CatalogARN": format!(
                        "arn:aws:glue:{REGION}:{}:catalog",
                        engine::emulator_account_id()
                    ),
                },
                "DestinationTableConfigurationList": [{
                    "DestinationDatabaseName": DATABASE,
                    "DestinationTableName": row.table,
                }],
                "S3Configuration": {
                    "RoleARN": role_arn,
                    "BucketARN": format!("arn:aws:s3:::{BUCKET}"),
                    "ErrorOutputPrefix": row.error_prefix(),
                },
                // The smallest size and the shortest interval the member admits, so a case
                // waits for the commit and not for a buffer window it did not ask to test.
                "BufferingHints": { "SizeInMBs": 1, "IntervalInSeconds": 0 },
            },
        }),
    )
    .await;
    assert!(
        answered["DeliveryStreamARN"]
            .as_str()
            .is_some_and(|arn| arn.ends_with(&format!("deliverystream/{stream}"))),
        "case {:?}: the create answers {stream}'s ARN, got {answered}",
        row.case
    );
}

/// Wait for the row's Firehose stream to report `ACTIVE` — the state a source-fed stream
/// retrieves in — and assert it reports the source it reads.
async fn await_active(router: &MultiServiceRouter, row: &Row, source_arn: &str) {
    let stream = row.stream();
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let described = firehose(
            router,
            row.case,
            "DescribeDeliveryStream",
            json!({ "DeliveryStreamName": stream }),
        )
        .await;
        let description = &described["DeliveryStreamDescription"];
        if description["DeliveryStreamStatus"] == json!("ACTIVE") {
            assert_eq!(
                description["Source"]["KinesisStreamSourceDescription"]["KinesisStreamARN"],
                json!(source_arn),
                "case {:?}: {stream} reports the source it reads, got {described}",
                row.case
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {:?}: {stream} must reach ACTIVE, got {described}",
            row.case
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The records the row's producer publishes: one JSON object per record, each value of the
/// type the destination table's column declares.
fn payloads(row: &Row) -> Vec<Vec<u8>> {
    (0..row.records)
        .map(|index| {
            json!({ "id": format!("{}-{index}", row.table), "seq": index as i64 })
                .to_string()
                .into_bytes()
        })
        .collect()
}

/// Publish the row's records to its source, the way the row names.
async fn publish(router: &MultiServiceRouter, row: &Row, records: &[Vec<u8>]) {
    let source = row.source();
    if row.batch {
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
            json!({ "StreamName": source, "Records": entries }),
        )
        .await;
        assert_eq!(
            answered["FailedRecordCount"].as_i64().unwrap_or(0),
            0,
            "case {:?}: the source takes every record of the batch, got {answered}",
            row.case
        );
        return;
    }
    for (index, payload) in records.iter().enumerate() {
        let answered = kinesis(
            router,
            row.case,
            "PutRecord",
            json!({
                "StreamName": source,
                "PartitionKey": format!("pk-{index}"),
                "Data": ::aws_smithy_types::base64::encode(payload),
            }),
        )
        .await;
        assert!(
            answered["SequenceNumber"].is_string(),
            "case {:?}: the source answers the sequence number it stored the record under, got \
             {answered}",
            row.case
        );
    }
}

/// Every key `BUCKET` holds under `prefix`, read through `ListObjectsV2`.
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

/// The bytes `BUCKET` holds at `key`.
async fn object(router: &MultiServiceRouter, case: &str, key: &str) -> Vec<u8> {
    let (status, body) = send(router, "s3", "GET", &format!("/{BUCKET}/{key}"), &[], Vec::new())
        .await;
    assert_eq!(
        status,
        200,
        "case {case:?}: reading {BUCKET}/{key} must succeed, got {status} {}",
        String::from_utf8_lossy(&body)
    );
    body
}

/// The document stored at the `s3://` location `pointer`.
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

/// The snapshot a metadata document reports as current.
fn current_snapshot(document: &Value) -> Option<&Value> {
    let current = document["current-snapshot-id"].clone();
    document["snapshots"]
        .as_array()?
        .iter()
        .find(|snapshot| snapshot["snapshot-id"] == current)
}

/// One snapshot summary total, as the document carries it (Iceberg states a summary's values
/// as strings).
fn total(snapshot: &Value, name: &str) -> i64 {
    snapshot["summary"][name]
        .as_str()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(-1)
}

/// The row's advanced catalog pointer and the metadata document it names, once the commits of
/// the row account for every record its producer published — or a failure once
/// [`DELIVERY_DEADLINE`] has passed.
///
/// The pointer is what is polled because the catalog is advanced only AFTER the objects of a
/// commit are written, so a pointer that moved is a snapshot whose objects are all there; and
/// the total is what is waited on because a buffer may be flushed in more than one piece, each
/// of which commits a snapshot of its own.
async fn committed(router: &MultiServiceRouter, row: &Row, before: &str) -> (String, Value) {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let pointer = table_parameter(router, row.case, row.table, METADATA_LOCATION)
            .await
            .unwrap_or_default();
        if pointer != before && !pointer.is_empty() {
            let document = document_at(router, row.case, &pointer).await;
            if let Some(snapshot) = current_snapshot(&document) {
                let records = i64::try_from(row.records).unwrap_or(i64::MAX);
                if total(snapshot, "total-records") == records {
                    return (pointer, document);
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "case {:?}: expected the destination table {} to hold a committed snapshot \
             accounting for all {} published record(s), its catalog pointer is {pointer:?} \
             (it was {before:?})",
            row.case,
            row.table,
            row.records
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: a source-fed stream whose destination is an Iceberg table commits the records it
/// consumed into the destination table the catalog holds Iceberg metadata for — whichever way
/// that metadata got onto the row — and writes nothing at all for a destination table it does
/// not.
#[tokio::test]
async fn a_source_fed_iceberg_destination_commits_only_into_a_marked_table() {
    let router = emulator();
    let rows = rows();
    given_bucket(&router).await;
    glue(
        &router,
        "setup",
        "CreateDatabase",
        json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;
    let role_arn = given_role(&router, "firehose-iceberg-source-fed-role").await;

    // The destination tables, and the catalog pointer each one carries BEFORE anything is
    // delivered: the marker the row was created with is exactly what decides whether a
    // delivery has metadata to commit onto.
    let mut before: Vec<String> = Vec::new();
    for row in &rows {
        given_table(&router, row).await;
        let pointer = table_parameter(&router, row.case, row.table, METADATA_LOCATION).await;
        assert_eq!(
            pointer.is_some(),
            row.commits,
            "case {:?}: the catalog holds a metadata location for this destination table \
             exactly when a delivery into it can commit, got {pointer:?}",
            row.case
        );
        before.push(pointer.unwrap_or_default());
    }

    // The sources, then the streams reading them: every create is made before any status is
    // waited for, so the clock-owed spans of the whole suite overlap.
    let mut sources: Vec<String> = Vec::new();
    for row in &rows {
        sources.push(given_source(&router, row).await);
    }
    for (row, source_arn) in rows.iter().zip(&sources) {
        create_stream(&router, row, source_arn, &role_arn).await;
    }
    for (row, source_arn) in rows.iter().zip(&sources) {
        await_active(&router, row, source_arn).await;
    }

    // The producers publish, each to its own source.
    let published: Vec<Vec<Vec<u8>>> = rows.iter().map(payloads).collect();
    for (row, records) in rows.iter().zip(&published) {
        publish(&router, row, records).await;
    }

    // THE MARKED TABLES COMMIT: the data object, the snapshot's two Avro objects, the metadata
    // file the catalog now points at, and a summary accounting for every published record.
    for (row, before) in rows.iter().zip(&before) {
        if !row.commits {
            continue;
        }
        let (pointer, document) = committed(&router, row, before).await;
        assert!(
            pointer.starts_with(&row.location()) && pointer.ends_with(".metadata.json"),
            "case {:?}: the advanced pointer names a metadata file under the table's own \
             location, got {pointer}",
            row.case
        );

        let data_keys = keys_under(&router, row.case, &format!("{}/data/", row.prefix())).await;
        assert!(
            !data_keys.is_empty(),
            "case {:?}: a commit writes its rows under the table's own data directory, got \
             {data_keys:?}",
            row.case
        );
        for key in &data_keys {
            assert!(
                key.ends_with(".parquet"),
                "case {:?}: the committed data objects are Parquet files, got {data_keys:?}",
                row.case
            );
            let body = object(&router, row.case, key).await;
            assert!(
                body.starts_with(b"PAR1"),
                "case {:?}: the data object {key} opens with the Parquet magic, got {:?}",
                row.case,
                body.get(..4)
            );
        }

        let metadata_keys =
            keys_under(&router, row.case, &format!("{}/metadata/", row.prefix())).await;
        let avro: Vec<&String> = metadata_keys
            .iter()
            .filter(|key| key.ends_with(".avro"))
            .collect();
        assert!(
            avro.iter().any(|key| key.contains("/snap-")),
            "case {:?}: a commit writes the manifest list of its snapshot, got \
             {metadata_keys:?}",
            row.case
        );
        assert!(
            avro.iter().any(|key| key.ends_with("-m0.avro")),
            "case {:?}: a commit writes the manifest of its data file, got {metadata_keys:?}",
            row.case
        );
        for key in &avro {
            let body = object(&router, row.case, key).await;
            assert!(
                body.starts_with(b"Obj"),
                "case {:?}: the Avro object {key} opens with its container magic, got {:?}",
                row.case,
                body.get(..4)
            );
        }

        let snapshot = current_snapshot(&document).unwrap_or_else(|| {
            panic!(
                "case {:?}: the metadata file must hold its current snapshot, got {document}",
                row.case
            )
        });
        assert_eq!(
            snapshot["summary"]["operation"].as_str(),
            Some("append"),
            "case {:?}: a delivery commits an append, got {snapshot}",
            row.case
        );
        assert!(
            total(snapshot, "total-data-files") >= 1,
            "case {:?}: the committed snapshot holds the data file(s) it wrote, got {snapshot}",
            row.case
        );
        assert!(
            snapshot["manifest-list"]
                .as_str()
                .is_some_and(|list| list.starts_with(&row.location()) && list.ends_with(".avro")),
            "case {:?}: the snapshot names the manifest list a reader resolves it through, got \
             {snapshot}",
            row.case
        );
        let errors = keys_under(&router, row.case, &row.error_prefix()).await;
        assert!(
            errors.is_empty(),
            "case {:?}: every published record was committed, so nothing is written under the \
             error output, got {errors:?}",
            row.case
        );
    }

    // THE UNMARKED TABLES TAKE NOTHING — read once the marked tables have demonstrably
    // committed, and then WATCHED, so a late write fails the case whenever it happens.
    let until = Instant::now() + WATCH;
    loop {
        for row in rows.iter().filter(|row| !row.commits) {
            let keys = keys_under(&router, row.case, &format!("{}/", row.prefix())).await;
            assert!(
                keys.is_empty(),
                "case {:?}: a destination table the catalog holds no Iceberg metadata for is \
                 committed nothing, got {keys:?}",
                row.case
            );
            let errors = keys_under(&router, row.case, &row.error_prefix()).await;
            assert!(
                errors.is_empty(),
                "case {:?}: nothing failed either, so the error output stays empty, got \
                 {errors:?}",
                row.case
            );
            assert_eq!(
                table_parameter(&router, row.case, row.table, METADATA_LOCATION).await,
                None,
                "case {:?}: a delivery never makes a table that was not an Iceberg table one",
                row.case
            );
        }
        if Instant::now() >= until {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
