// SPDX-License-Identifier: CC0-1.0

//! The destination STREAMING TABLE a channel delivers to, end to end over the wire, with
//! Kinesis and S3 Tables mounted on ONE engine — the shape `core/router/src/lib.rs` mounts
//! them in — so the table a channel's create provisions is a record S3 Tables' own handlers
//! serve.
//!
//! RULE: a `CreateChannel` whose destination is a streaming table provisions that table in S3
//! Tables as it stores the channel — "Amazon Kinesis Data Streams creates this table in the
//! specified table bucket" (`S3TablesConfiguration$TableName`) — in the table bucket and
//! namespace the configuration names, under the name it names, reported by S3 Tables as a
//! table a service manages (`GetTable$managedByService` = this service's principal,
//! `GetTable$type` = `aws`), so `GetTable` and `ListTables` resolve it exactly as they resolve
//! a table a caller created. Provisioning never CLAIMS a table it did not create: a
//! destination naming a table bucket or namespace this account and region do not hold has
//! nowhere for its table and gets none, and a name a caller's own table holds is left to that
//! caller — a table name is unique inside its namespace.
//!
//! RULE 2: that table is created partitioned the way its configuration asks for ("Specifies how
//! the destination table is partitioned", `smithy_query(query="shape",shape="PartitionSpec")`),
//! so a create whose `S3TablesConfigurationList` holds a configuration naming no `PartitionSpec`
//! describes a table that cannot be created and is refused — HTTP 400, `ValidationException`,
//! "PartitionSpec is required." — before any channel is stored and before any table is
//! provisioned, so the name it asked for holds no channel and the destination holds no table. A
//! configuration that does name one is accepted whatever that specification holds, reaches
//! `ACTIVE`, and reports the partitioning it was created with.
//!
//! RULE 3: that destination delivers on the data-freshness window the service documents a
//! range for — 300 to 900 seconds, both ends served — so a `CreateChannel` or an
//! `UpdateChannel` naming a window outside that range is refused — HTTP 400,
//! `ValidationException`, "DataFreshnessInSeconds must be between 300 and 900." — before any
//! channel is stored and before any table is provisioned, while a window inside it, and a
//! create that names none and takes the documented default, is served and reported back by
//! every later read.
//!
//! The recorded flow this was derived from creates ONE channel with ONE streaming table
//! configuration and then reads that table back, which is one row of the table below. The
//! rows differ along what the rule generalises over: the form and length of the destination
//! names (short, underscored, at the modelled `@length` bound of `S3TablesTableName` and
//! `S3TablesNamespace`), the compression the destination asks for, the record format the
//! source stream carries, and which bucket and namespace of the account the destination
//! names.
//!
//! Every Kinesis case drives the real awsJson1_1 request path (`POST /` with `X-Amz-Target:
//! Kinesis_20131202.<Operation>`) and every S3 Tables case its restJson1 one, through
//! `MultiServiceRouter::dispatch`, so the statuses, bodies and modelled errors asserted here
//! are the ones a client sees.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_kinesis_kinesis_20131202::state::{
    channel_arn, managing_service_principal, CHANNEL_ACTIVE, STREAM_ACTIVE, STREAM_MODE_ON_DEMAND,
};
use service_com_amazonaws_kinesis_kinesis_20131202::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::channel_lifecycle::{dead_letter_queue, role_arn, schema_arn, stream_configuration};
use crate::stream_tags::{error_code, error_message, poll_until, stream_arn, REGION};

/// The `X-Amz-Target` prefix of this service (`OPERATIONS.md`, "Wire protocol").
const TARGET_PREFIX: &str = "Kinesis_20131202";

/// The `type` S3 Tables reports for "a table Amazon Web Services created on the account's
/// behalf" (`smithy_query(query="shape",shape="TableType")` -> `aws | customer`).
const TABLE_TYPE_AWS: &str = "aws";

/// The `@length` maximum of a destination table's name and of its namespace
/// (`smithy_query(query="shape",shape="S3TablesConfiguration")` -> `TableName` and
/// `Namespace`, both len 1..255, pattern `^[0-9a-z_]+$`).
const DESTINATION_NAME_MAX_LENGTH: usize = 255;

/// The error this service answers for an input it validates before it provisions anything.
const VALIDATION: &str = "ValidationException";

/// The sentence the service answers for a streaming table configuration that names no
/// partitioning specification. FIXED text, written out here rather than borrowed from the crate,
/// so a drift away from it fails rather than comparing the handler against itself.
const PARTITION_SPEC_REQUIRED: &str = "PartitionSpec is required.";

/// Kinesis and S3 Tables over ONE engine, so the table bucket a destination names and the
/// table the create provisions in it are the records S3 Tables' own handlers hold.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The signature itself is not under test here, so requests are unauthenticated; the
        // router still routes by the SigV4 credential scope, which is where the request's
        // region comes from.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_s3tables_s3tablebuckets::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// One Kinesis call, signed for [`REGION`].
    async fn call(&self, operation: &str, body: &Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("kinesis.{REGION}.amazonaws.com"))
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/kinesis/\
                     aws4_request, SignedHeaders=host;x-amz-target, Signature=deadbeef"
                ),
            )
            .body(Bytes::from(body.to_string()))
            .expect("build the request");
        self.dispatch(request).await
    }

    /// One Kinesis call that must be answered 200.
    async fn ok(&self, case: &str, operation: &str, body: &Value) -> Value {
        let (status, response) = self.call(operation, body).await;
        assert_eq!(
            status, 200,
            "case {case}: expected {operation} to answer 200, got {status} with body {response}"
        );
        response
    }

    /// One S3 Tables call, signed for [`REGION`].
    async fn tables(&self, method: &str, path: &str, body: Option<&Value>) -> (u16, Value) {
        let payload = body.map_or_else(Vec::new, |value| value.to_string().into_bytes());
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://s3tables.{REGION}.amazonaws.com{path}"))
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/s3tables/\
                     aws4_request, SignedHeaders=host, Signature=deadbeef"
                ),
            )
            .body(Bytes::from(payload))
            .expect("build the request");
        self.dispatch(request).await
    }

    /// One S3 Tables call that must be answered 200.
    async fn tables_ok(&self, case: &str, method: &str, path: &str, body: Option<&Value>) -> Value {
        let (status, response) = self.tables(method, path, body).await;
        assert_eq!(
            status, 200,
            "case {case}: expected {method} {path} to answer 200, got {status} with body \
             {response}"
        );
        response
    }

    async fn dispatch(&self, request: http::Request<Bytes>) -> (u16, Value) {
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
        (status, parsed)
    }

    /// A table bucket of this account and region named `name`, as its ARN.
    async fn table_bucket(&self, case: &str, name: &str) -> String {
        let created = self
            .tables_ok(case, "PUT", "/buckets", Some(&json!({ "name": name })))
            .await;
        created["arn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: CreateTableBucket answers the arn, got {created}"))
            .to_string()
    }

    /// A namespace named `namespace` inside the table bucket `bucket_arn`.
    async fn namespace(&self, case: &str, bucket_arn: &str, namespace: &str) {
        self.tables_ok(
            case,
            "PUT",
            &format!("/namespaces/{}", encode(bucket_arn)),
            Some(&json!({ "namespace": [namespace] })),
        )
        .await;
    }

    /// `GetTable` for one table, addressed by its bucket, namespace and name.
    async fn get_table(
        &self,
        bucket_arn: &str,
        namespace: &str,
        name: &str,
    ) -> (u16, Value) {
        self.tables(
            "GET",
            &format!(
                "/get-table?tableBucketARN={}&namespace={}&name={}",
                encode(bucket_arn),
                encode(namespace),
                encode(name)
            ),
            None,
        )
        .await
    }

    /// The `tables` member of `ListTables` for one bucket and namespace.
    async fn list_tables(&self, case: &str, bucket_arn: &str, namespace: &str) -> Vec<Value> {
        let listed = self
            .tables_ok(
                case,
                "GET",
                &format!(
                    "/tables/{}?namespace={}",
                    encode(bucket_arn),
                    encode(namespace)
                ),
                None,
            )
            .await;
        listed["tables"]
            .as_array()
            .cloned()
            .unwrap_or_else(|| panic!("case {case}: ListTables answers a tables list, got {listed}"))
    }

    /// An `ACTIVE` on-demand stream named `name` — the only capacity mode a channel supports
    /// — as its ARN.
    async fn on_demand_stream(&self, case: &str, name: &str) -> String {
        self.ok(
            case,
            "CreateStream",
            &json!({
                "StreamName": name,
                "StreamModeDetails": { "StreamMode": STREAM_MODE_ON_DEMAND },
            }),
        )
        .await;
        poll_until(case, &format!("the stream {name} is ready to serve"), || async {
            let (status, response) = self
                .call("DescribeStreamSummary", &json!({ "StreamName": name }))
                .await;
            status == 200
                && response["StreamDescriptionSummary"]["StreamStatus"].as_str()
                    == Some(STREAM_ACTIVE)
        })
        .await;
        stream_arn(REGION, name)
    }
}

/// An `@httpLabel` is ONE path segment and a query value ends at the next `&`, so an ARN's
/// `:` and `/` have to travel percent-encoded or the request addresses a different route.
fn encode(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace(':', "%3A")
        .replace('/', "%2F")
        .replace('&', "%26")
        .replace('=', "%3D")
}

/// `name` repeated to `length` characters inside the `^[0-9a-z_]+$` pattern both destination
/// names carry, for the rows that sit on a modelled bound.
fn name_of_length(seed: &str, length: usize) -> String {
    seed.chars().cycle().take(length).collect()
}

/// The one transform a partition field may ask for: "The partition transform to apply. The only
/// valid value is TIME_HOUR" (`smithy_query(query="shape",shape="PartitionField")` ->
/// `Transform`).
const PARTITION_TRANSFORM: &str = "TIME_HOUR";

/// The most partition fields one specification may carry
/// (`smithy_query(query="shape",shape="PartitionSpec")` -> `PartitionFields`, len 1..10).
const MAX_PARTITION_FIELDS: usize = 10;

/// A `PartitionSpec` partitioning the destination table on `sources`, one field per source
/// column.
fn partition_spec(sources: &[&str]) -> Value {
    let fields: Vec<Value> = sources
        .iter()
        .map(|source| json!({ "Transform": PARTITION_TRANSFORM, "SourceName": source }))
        .collect();
    json!({ "PartitionFields": fields })
}

/// One streaming table configuration, carrying a `PartitionSpec` only where `spec` names one.
fn table_configuration(
    bucket_arn: &str,
    namespace: &str,
    table: &str,
    compression: &str,
    spec: Option<Value>,
) -> Value {
    let mut configuration = json!({
        "TableBucketARN": bucket_arn,
        "Namespace": namespace,
        "TableName": table,
        "CompressionType": compression,
    });
    if let (Some(members), Some(spec)) = (configuration.as_object_mut(), spec) {
        members.insert("PartitionSpec".to_string(), spec);
    }
    configuration
}

/// One streaming table destination delivering to `configurations`, with the dead-letter queue
/// the model makes required.
fn tables_destination_of(configurations: Vec<Value>) -> Value {
    json!({
        "DeadLetterQueueS3Configuration": dead_letter_queue("channel-dlq", "tables/"),
        "S3TablesConfigurationList": configurations,
    })
}

/// One streaming table destination, partitioned on a single source column.
fn tables_destination(bucket_arn: &str, namespace: &str, table: &str, compression: &str) -> Value {
    tables_destination_of(vec![table_configuration(
        bucket_arn,
        namespace,
        table,
        compression,
        Some(partition_spec(&["written_at"])),
    )])
}

/// The `CreateChannel` request body for a channel delivering to `destination`.
fn create_channel_body(name: &str, stream_arn: &str, format: &str, destination: Value) -> Value {
    json!({
        "ChannelName": name,
        "ServiceExecutionRoleARN": role_arn("channel-delivery"),
        "StreamConfigurationList": [stream_configuration(
            stream_arn,
            format,
            Some(schema_arn("records", "iceberg")),
        )],
        "S3TablesDestinationConfiguration": destination,
    })
}

/// One row of the provisioning table: a channel whose destination names `bucket`, `namespace`
/// and `table`.
struct Row {
    case: &'static str,
    bucket: &'static str,
    namespace: String,
    table: String,
    compression: &'static str,
    record_format: &'static str,
}

/// A create whose destination is a streaming table creates that table in S3 Tables, under the
/// destination's own bucket, namespace and name, reported as managed by this service — and
/// `ListTables` on that bucket and namespace reports exactly it.
#[tokio::test]
async fn a_streaming_table_destination_creates_the_table_it_names() {
    let emulator = Emulator::new();
    let rows = [
        Row {
            case: "short names",
            bucket: "ch-tables-short",
            namespace: "events".to_string(),
            table: "records".to_string(),
            compression: "ZSTD",
            record_format: "GSR_JSON",
        },
        Row {
            case: "underscored names",
            bucket: "ch-tables-underscored",
            namespace: "raw_events_2026".to_string(),
            table: "clickstream_v2".to_string(),
            compression: "SNAPPY",
            record_format: "JSON",
        },
        Row {
            case: "names at the modelled length bound",
            bucket: "ch-tables-bound",
            namespace: name_of_length("ns_0", DESTINATION_NAME_MAX_LENGTH),
            table: name_of_length("t_9", DESTINATION_NAME_MAX_LENGTH),
            compression: "NONE",
            record_format: "GSR_JSON",
        },
        Row {
            case: "names at the modelled length minimum",
            bucket: "ch-tables-minimum",
            namespace: "n".to_string(),
            table: "t".to_string(),
            compression: "ZSTD",
            record_format: "JSON",
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let bucket_arn = emulator.table_bucket(case, row.bucket).await;
        emulator.namespace(case, &bucket_arn, &row.namespace).await;
        let source = emulator
            .on_demand_stream(case, &format!("tables-source-{index}"))
            .await;

        // Nothing holds the destination table before the channel is created, so what a read
        // resolves afterwards is what the create provisioned.
        let (before, body) = emulator
            .get_table(&bucket_arn, &row.namespace, &row.table)
            .await;
        assert_eq!(
            before, 404,
            "case {case}: the destination table must not exist before the create, got {before} \
             with body {body}"
        );

        let channel_name = format!("tables-channel-{index}");
        let created = emulator
            .ok(
                case,
                "CreateChannel",
                &create_channel_body(
                    &channel_name,
                    &source,
                    row.record_format,
                    tables_destination(&bucket_arn, &row.namespace, &row.table, row.compression),
                ),
            )
            .await;
        let channel_arn = created["ChannelDescription"]["ChannelARN"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: the create answers a ChannelARN, got {created}"))
            .to_string();
        poll_until(
            case,
            &format!("the channel {channel_arn} reports {CHANNEL_ACTIVE}"),
            || async {
                let (status, response) = emulator
                    .call("DescribeChannel", &json!({ "ChannelARN": channel_arn }))
                    .await;
                status == 200
                    && response["ChannelDescription"]["ChannelStatus"].as_str()
                        == Some(CHANNEL_ACTIVE)
            },
        )
        .await;

        let (status, table) = emulator
            .get_table(&bucket_arn, &row.namespace, &row.table)
            .await;
        assert_eq!(
            status, 200,
            "case {case}: the destination table must be readable after the create, got {status} \
             with body {table}"
        );
        assert_eq!(
            table["name"].as_str(),
            Some(row.table.as_str()),
            "case {case}: expected the table name {:?}, got {:?}",
            row.table,
            table["name"]
        );
        assert_eq!(
            table["namespace"],
            json!([row.namespace]),
            "case {case}: expected the namespace {:?}, got {:?}",
            json!([row.namespace]),
            table["namespace"]
        );
        assert_eq!(
            table["managedByService"].as_str(),
            Some(managing_service_principal().as_str()),
            "case {case}: expected managedByService {:?}, got {:?}",
            managing_service_principal(),
            table["managedByService"]
        );
        assert_eq!(
            table["type"].as_str(),
            Some(TABLE_TYPE_AWS),
            "case {case}: expected the table type {TABLE_TYPE_AWS:?}, got {:?}",
            table["type"]
        );

        let listed = emulator.list_tables(case, &bucket_arn, &row.namespace).await;
        let names: Vec<Option<&str>> = listed
            .iter()
            .map(|summary| summary["name"].as_str())
            .collect();
        assert_eq!(
            names,
            vec![Some(row.table.as_str())],
            "case {case}: expected the namespace to hold exactly the destination table, got \
             {names:?}"
        );
        assert_eq!(
            listed[0]["managedByService"].as_str(),
            Some(managing_service_principal().as_str()),
            "case {case}: expected the listed table to report managedByService {:?}, got {:?}",
            managing_service_principal(),
            listed[0]["managedByService"]
        );
    }
}

/// A table a CALLER created through S3 Tables is managed by no service, so the member that
/// reports a managing service tells the two apart inside one namespace.
#[tokio::test]
async fn a_caller_created_table_beside_it_reports_no_managing_service() {
    let case = "a customer table beside a channel's own";
    let emulator = Emulator::new();
    let bucket_arn = emulator.table_bucket(case, "ch-tables-mixed").await;
    emulator.namespace(case, &bucket_arn, "events").await;
    emulator
        .tables_ok(
            case,
            "PUT",
            &format!(
                "/tables/{}/{}",
                encode(&bucket_arn),
                encode("events")
            ),
            Some(&json!({ "name": "own_table", "format": "ICEBERG" })),
        )
        .await;
    let source = emulator.on_demand_stream(case, "mixed-source").await;
    emulator
        .ok(
            case,
            "CreateChannel",
            &create_channel_body(
                "mixed-channel",
                &source,
                "GSR_JSON",
                tables_destination(&bucket_arn, "events", "channel_table", "ZSTD"),
            ),
        )
        .await;

    for (table, expected) in [
        ("channel_table", Some(managing_service_principal())),
        ("own_table", None),
    ] {
        let (status, body) = emulator.get_table(&bucket_arn, "events", table).await;
        assert_eq!(
            status, 200,
            "case {case}: expected {table} to be readable, got {status} with body {body}"
        );
        let reported = body["managedByService"].as_str().map(str::to_string);
        assert_eq!(
            reported, expected,
            "case {case}: expected {table} to report managedByService {expected:?}, got \
             {reported:?}"
        );
    }
    let listed = emulator.list_tables(case, &bucket_arn, "events").await;
    let reported: Vec<(String, Option<String>)> = listed
        .iter()
        .map(|summary| {
            (
                summary["name"].as_str().unwrap_or_default().to_string(),
                summary["managedByService"].as_str().map(str::to_string),
            )
        })
        .collect();
    let expected: Vec<(String, Option<String>)> = vec![
        ("channel_table".to_string(), Some(managing_service_principal())),
        ("own_table".to_string(), None),
    ];
    assert_eq!(
        reported, expected,
        "case {case}: expected the listing to report {expected:?}, got {reported:?}"
    );
}

/// One row of the table of destinations S3 Tables cannot take a table for: what of the
/// destination exists before the channel is created.
struct Unprovisionable {
    case: &'static str,
    /// Whether the table bucket is created at all.
    create_bucket: bool,
    /// Whether the destination namespace is created inside it.
    create_namespace: bool,
}

/// A destination S3 Tables has nowhere to put its table in leaves that destination WITHOUT a
/// table — and the emulator claims none: every read of it answers what S3 Tables actually
/// holds, which is nothing.
#[tokio::test]
async fn a_destination_with_nowhere_for_its_table_is_never_claimed_to_have_one() {
    let rows = [
        Unprovisionable {
            case: "a table bucket this account does not hold",
            create_bucket: false,
            create_namespace: false,
        },
        Unprovisionable {
            case: "a namespace the table bucket does not hold",
            create_bucket: true,
            create_namespace: false,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let emulator = Emulator::new();
        let bucket = format!("ch-tables-absent-{index}");
        let bucket_arn = if row.create_bucket {
            emulator.table_bucket(case, &bucket).await
        } else {
            format!(
                "arn:aws:s3tables:{REGION}:{}:bucket/{bucket}",
                engine::EMULATOR_ACCOUNT_ID
            )
        };
        if row.create_namespace {
            emulator.namespace(case, &bucket_arn, "events").await;
        }
        let source = emulator
            .on_demand_stream(case, &format!("absent-source-{index}"))
            .await;

        emulator
            .ok(
                case,
                "CreateChannel",
                &create_channel_body(
                    &format!("absent-destination-channel-{index}"),
                    &source,
                    "GSR_JSON",
                    tables_destination(&bucket_arn, "events", "records", "ZSTD"),
                ),
            )
            .await;

        let (status, body) = emulator.get_table(&bucket_arn, "events", "records").await;
        assert_eq!(
            status, 404,
            "case {case}: expected the destination table to be reported as absent, got {status} \
             with body {body}"
        );
    }
}

/// A table a CALLER created is never taken over: a destination naming it leaves the caller's
/// record exactly as it was, still managed by no service.
#[tokio::test]
async fn a_caller_created_table_is_not_taken_over_by_a_destination_of_its_name() {
    let case = "a destination naming a caller's own table";
    let emulator = Emulator::new();
    let bucket_arn = emulator.table_bucket(case, "ch-tables-held").await;
    emulator.namespace(case, &bucket_arn, "events").await;
    let created = emulator
        .tables_ok(
            case,
            "PUT",
            &format!("/tables/{}/{}", encode(&bucket_arn), encode("events")),
            Some(&json!({ "name": "records", "format": "ICEBERG" })),
        )
        .await;
    let held_arn = created["tableARN"].as_str().unwrap_or_default().to_string();
    let source = emulator.on_demand_stream(case, "held-source").await;

    emulator
        .ok(
            case,
            "CreateChannel",
            &create_channel_body(
                "held-destination-channel",
                &source,
                "GSR_JSON",
                tables_destination(&bucket_arn, "events", "records", "ZSTD"),
            ),
        )
        .await;

    let (status, table) = emulator.get_table(&bucket_arn, "events", "records").await;
    assert_eq!(
        status, 200,
        "case {case}: the caller's own table must still be readable, got {status} with body \
         {table}"
    );
    assert_eq!(
        table["tableARN"].as_str(),
        Some(held_arn.as_str()),
        "case {case}: expected the caller's table {held_arn:?} to be untouched, got {:?}",
        table["tableARN"]
    );
    assert_eq!(
        table["managedByService"].as_str(),
        None,
        "case {case}: a caller's own table is managed by no service, got {:?}",
        table["managedByService"]
    );
    let listed = emulator.list_tables(case, &bucket_arn, "events").await;
    assert_eq!(
        listed.len(),
        1,
        "case {case}: expected the namespace to still hold exactly the caller's table, got \
         {listed:?}"
    );
}

/// Provisioning is idempotent for the tables this service manages: a second channel delivering
/// to the same destination reaches the same one table rather than a refusal or a second
/// record.
#[tokio::test]
async fn a_second_channel_to_one_destination_reaches_the_same_table() {
    let case = "two channels, one destination table";
    let emulator = Emulator::new();
    let bucket_arn = emulator.table_bucket(case, "ch-tables-shared").await;
    emulator.namespace(case, &bucket_arn, "events").await;
    let source = emulator.on_demand_stream(case, "shared-source").await;

    let mut arns = Vec::new();
    for channel in ["first-channel", "second-channel"] {
        emulator
            .ok(
                case,
                "CreateChannel",
                &create_channel_body(
                    channel,
                    &source,
                    "GSR_JSON",
                    tables_destination(&bucket_arn, "events", "records", "ZSTD"),
                ),
            )
            .await;
        let (status, table) = emulator.get_table(&bucket_arn, "events", "records").await;
        assert_eq!(
            status, 200,
            "case {case}: the destination table must be readable after {channel}, got {status} \
             with body {table}"
        );
        arns.push(table["tableARN"].as_str().map(str::to_string));
    }
    assert_eq!(
        arns[0], arns[1],
        "case {case}: expected both creates to reach one table, got {:?} then {:?}",
        arns[0], arns[1]
    );
    let listed = emulator.list_tables(case, &bucket_arn, "events").await;
    assert_eq!(
        listed.len(),
        1,
        "case {case}: expected the namespace to hold exactly one table, got {listed:?}"
    );
}

/// One row of the unpartitioned table: the configurations a destination carries, and the table
/// of them whose absence afterwards proves nothing was provisioned.
struct Unpartitioned {
    case: &'static str,
    /// `(namespace, table, compression, whether it names a PartitionSpec)` per configuration.
    configurations: Vec<(&'static str, &'static str, &'static str, bool)>,
    record_format: &'static str,
}

/// A create whose streaming table destination holds a configuration naming no partitioning
/// specification is refused with the modelled `ValidationException`, and leaves behind neither a
/// channel nor a table.
#[tokio::test]
async fn a_configuration_without_a_partition_spec_is_refused_and_provisions_nothing() {
    let emulator = Emulator::new();
    let bucket_arn = emulator
        .table_bucket("partitioning", "ch-tables-partitioning")
        .await;
    for namespace in ["events", "raw_events_2026"] {
        emulator.namespace("partitioning", &bucket_arn, namespace).await;
    }
    let source = emulator.on_demand_stream("partitioning", "partitioning-source").await;

    // The rows differ along which configuration of the list names no specification, and along
    // the destination names, the compression and the record format around it.
    let rows = [
        Unpartitioned {
            case: "the only configuration names no spec",
            configurations: vec![("events", "records", "ZSTD", false)],
            record_format: "GSR_JSON",
        },
        Unpartitioned {
            case: "another namespace, table and compression, still no spec",
            configurations: vec![("raw_events_2026", "clickstream_v2", "NONE", false)],
            record_format: "JSON",
        },
        Unpartitioned {
            case: "the second of two configurations names no spec",
            configurations: vec![
                ("events", "first_records", "SNAPPY", true),
                ("events", "second_records", "SNAPPY", false),
            ],
            record_format: "GSR_JSON",
        },
        Unpartitioned {
            case: "the first of two configurations names no spec",
            configurations: vec![
                ("events", "third_records", "ZSTD", false),
                ("events", "fourth_records", "ZSTD", true),
            ],
            record_format: "JSON",
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let configurations: Vec<Value> = row
            .configurations
            .iter()
            .map(|(namespace, table, compression, partitioned)| {
                table_configuration(
                    &bucket_arn,
                    namespace,
                    table,
                    compression,
                    partitioned.then(|| partition_spec(&["written_at"])),
                )
            })
            .collect();
        let name = format!("unpartitioned-channel-{index}");
        let (status, response) = emulator
            .call(
                "CreateChannel",
                &create_channel_body(
                    &name,
                    &source,
                    row.record_format,
                    tables_destination_of(configurations),
                ),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case}: expected a client error, got {status} with body {response}"
        );
        assert_eq!(
            error_code(&response),
            VALIDATION,
            "case {case}: expected {VALIDATION}, got {response}"
        );
        let message = error_message(&response);
        assert_eq!(
            message, PARTITION_SPEC_REQUIRED,
            "case {case}: expected the message {PARTITION_SPEC_REQUIRED:?}, got {message:?}"
        );

        // The refusal is taken before anything is persisted, so the name it asked for holds no
        // channel ...
        let (status, described) = emulator
            .call(
                "DescribeChannel",
                &json!({ "ChannelARN": channel_arn(REGION, &name) }),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case}: expected the refused create to have stored no channel, got {status} \
             with body {described}"
        );
        assert_eq!(
            error_code(&described),
            "ResourceNotFoundException",
            "case {case}: expected the refused create to have stored no channel, got {described}"
        );

        // ... and S3 Tables holds no table of ANY configuration the refused request named,
        // including the one that did name a specification.
        for (namespace, table, _, _) in &row.configurations {
            let (status, body) = emulator.get_table(&bucket_arn, namespace, table).await;
            assert_eq!(
                status, 404,
                "case {case}: expected the refused create to have provisioned no table {table}, \
                 got {status} with body {body}"
            );
        }
    }
}

/// One row of the partitioned table: the destination a create names and the partitioning it asks
/// for.
struct Partitioned {
    case: &'static str,
    namespace: &'static str,
    table: &'static str,
    compression: &'static str,
    record_format: &'static str,
    sources: Vec<&'static str>,
}

/// A configuration that DOES name its partitioning is accepted whatever that specification holds,
/// reports it back, reaches `ACTIVE`, and gets its table.
#[tokio::test]
async fn a_configuration_with_a_partition_spec_is_accepted_whatever_its_fields() {
    let emulator = Emulator::new();
    let bucket_arn = emulator
        .table_bucket("partitioned", "ch-tables-partitioned")
        .await;
    let rows = [
        Partitioned {
            case: "one partition field",
            namespace: "events",
            table: "records",
            compression: "ZSTD",
            record_format: "GSR_JSON",
            sources: vec!["written_at"],
        },
        Partitioned {
            case: "two partition fields on another destination",
            namespace: "raw_events_2026",
            table: "clickstream_v2",
            compression: "SNAPPY",
            record_format: "JSON",
            sources: vec!["written_at", "ingested.at_utc"],
        },
        Partitioned {
            case: "partition fields at the modelled count bound",
            namespace: "n",
            table: "t",
            compression: "NONE",
            record_format: "GSR_JSON",
            sources: vec![
                "ts_0", "ts_1", "ts_2", "ts_3", "ts_4", "ts_5", "ts_6", "ts_7", "ts_8", "ts_9",
            ],
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        assert!(
            !row.sources.is_empty() && row.sources.len() <= MAX_PARTITION_FIELDS,
            "case {case}: a row stays inside the modelled 1..{MAX_PARTITION_FIELDS} field count, \
             got {}",
            row.sources.len()
        );
        emulator.namespace(case, &bucket_arn, row.namespace).await;
        let source = emulator
            .on_demand_stream(case, &format!("partitioned-source-{index}"))
            .await;
        let spec = partition_spec(&row.sources);

        let created = emulator
            .ok(
                case,
                "CreateChannel",
                &create_channel_body(
                    &format!("partitioned-channel-{index}"),
                    &source,
                    row.record_format,
                    tables_destination_of(vec![table_configuration(
                        &bucket_arn,
                        row.namespace,
                        row.table,
                        row.compression,
                        Some(spec.clone()),
                    )]),
                ),
            )
            .await;
        let reported = &created["ChannelDescription"]["S3TablesDestinationConfiguration"]
            ["S3TablesConfigurationList"][0]["PartitionSpec"];
        assert_eq!(
            reported, &spec,
            "case {case}: expected the channel to report the partitioning {spec}, got {reported}"
        );

        let channel_arn = created["ChannelDescription"]["ChannelARN"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: the create answers a ChannelARN, got {created}"))
            .to_string();
        poll_until(
            case,
            &format!("the channel {channel_arn} reports {CHANNEL_ACTIVE}"),
            || async {
                let (status, response) = emulator
                    .call("DescribeChannel", &json!({ "ChannelARN": channel_arn }))
                    .await;
                status == 200
                    && response["ChannelDescription"]["ChannelStatus"].as_str()
                        == Some(CHANNEL_ACTIVE)
            },
        )
        .await;

        let described = emulator
            .ok(
                case,
                "DescribeChannel",
                &json!({ "ChannelARN": channel_arn }),
            )
            .await;
        let read_back = &described["ChannelDescription"]["S3TablesDestinationConfiguration"]
            ["S3TablesConfigurationList"][0]["PartitionSpec"];
        assert_eq!(
            read_back, &spec,
            "case {case}: expected a read of the channel to report the partitioning {spec}, got \
             {read_back}"
        );

        let (status, table) = emulator
            .get_table(&bucket_arn, row.namespace, row.table)
            .await;
        assert_eq!(
            status, 200,
            "case {case}: expected the accepted create to have provisioned its table, got \
             {status} with body {table}"
        );
    }
}

/// The shortest data-freshness window a streaming table destination delivers on. The model
/// states no bound — `smithy_query(query="shape",shape="S3TablesDestinationConfiguration")` ->
/// `DataFreshnessInSeconds` targets a plain integer whose documentation names only the default
/// — so this is the lower end of the delivery range the service documents for a streaming table
/// destination.
const MIN_FRESHNESS_SECONDS: i64 = 300;

/// The longest window it delivers on: the upper end of that same documented range.
const MAX_FRESHNESS_SECONDS: i64 = 900;

/// The window a destination that names none gets: "The default value is 300 seconds" (same
/// member).
const DEFAULT_FRESHNESS_SECONDS: i64 = 300;

/// The sentence the service refuses a window outside that range with. FIXED text around the
/// member's own two bounds, written out here rather than borrowed from the crate, so a drift
/// away from the wording fails rather than comparing the handler against itself.
const FRESHNESS_OUT_OF_RANGE: &str = "DataFreshnessInSeconds must be between 300 and 900.";

/// One streaming table destination delivering to one table, naming `freshness` as its
/// data-freshness window only where a case states one.
fn tables_destination_with_freshness(
    bucket_arn: &str,
    namespace: &str,
    table: &str,
    freshness: Option<i64>,
) -> Value {
    let mut destination = tables_destination(bucket_arn, namespace, table, "ZSTD");
    if let (Some(members), Some(freshness)) = (destination.as_object_mut(), freshness) {
        members.insert("DataFreshnessInSeconds".to_string(), json!(freshness));
    }
    destination
}

/// The window the stored channel `arn` reports for its streaming table destination.
async fn reported_freshness(emulator: &Emulator, case: &str, arn: &str) -> Option<i64> {
    let read = emulator
        .ok(case, "DescribeChannel", &json!({ "ChannelARN": arn }))
        .await;
    read["ChannelDescription"]["S3TablesDestinationConfiguration"]["DataFreshnessInSeconds"]
        .as_i64()
}

/// Look at the channel `arn` until it reports the `ACTIVE` its create or its last accepted
/// change owes it, through the crate's one poll loop ([`crate::stream_tags::poll_until`]).
///
/// The sibling `crate::channel_lifecycle::poll_until_channel_active` keeps the same loop for
/// the Kinesis-only harness, and takes THAT suite's `Emulator`; the cases here drive the
/// Kinesis-and-S3-Tables router this file mounts, so the wait is written once for this
/// `Emulator` rather than threaded through a second harness.
async fn poll_until_active(emulator: &Emulator, case: &str, arn: &str) {
    poll_until(
        case,
        &format!("the channel {arn} reports {CHANNEL_ACTIVE}"),
        || async {
            let (status, response) = emulator
                .call("DescribeChannel", &json!({ "ChannelARN": arn }))
                .await;
            status == 200
                && response["ChannelDescription"]["ChannelStatus"].as_str() == Some(CHANNEL_ACTIVE)
        },
    )
    .await;
}

/// One row of the freshness table: the window a create names (absent where `None`) and the
/// window the channel must report, or `None` where the create must be refused.
struct Freshness {
    case: &'static str,
    freshness: Option<i64>,
    served: Option<i64>,
}

/// A create takes a streaming table destination's data-freshness window only inside the range
/// the destination delivers on, and a refused one leaves behind neither a channel, nor a table,
/// nor an entry on the source stream's listing.
#[tokio::test]
async fn a_streaming_table_window_is_served_only_inside_the_documented_range() {
    let emulator = Emulator::new();
    let bucket_arn = emulator.table_bucket("freshness", "ch-tables-freshness").await;
    emulator.namespace("freshness", &bucket_arn, "events").await;
    let source = emulator
        .on_demand_stream("freshness", "freshness-source")
        .await;

    // The rows differ along where the window sits relative to the range: under its lower end,
    // on each published bound, inside, over its upper end, and absent altogether.
    let rows = [
        Freshness {
            case: "one second under the minimum",
            freshness: Some(MIN_FRESHNESS_SECONDS - 1),
            served: None,
        },
        Freshness {
            case: "a zero window",
            freshness: Some(0),
            served: None,
        },
        Freshness {
            case: "a negative window",
            freshness: Some(-1),
            served: None,
        },
        Freshness {
            case: "one second over the maximum",
            freshness: Some(MAX_FRESHNESS_SECONDS + 1),
            served: None,
        },
        Freshness {
            case: "a day-long window",
            freshness: Some(86_400),
            served: None,
        },
        Freshness {
            case: "the minimum itself",
            freshness: Some(MIN_FRESHNESS_SECONDS),
            served: Some(MIN_FRESHNESS_SECONDS),
        },
        Freshness {
            case: "inside the range",
            freshness: Some(451),
            served: Some(451),
        },
        Freshness {
            case: "the maximum itself",
            freshness: Some(MAX_FRESHNESS_SECONDS),
            served: Some(MAX_FRESHNESS_SECONDS),
        },
        Freshness {
            case: "no window named, so the documented default",
            freshness: None,
            served: Some(DEFAULT_FRESHNESS_SECONDS),
        },
    ];

    let mut channels_served: Vec<String> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let name = format!("freshness-channel-{index}");
        let table = format!("freshness_records_{index}");
        let (status, response) = emulator
            .call(
                "CreateChannel",
                &create_channel_body(
                    &name,
                    &source,
                    "GSR_JSON",
                    tables_destination_with_freshness(&bucket_arn, "events", &table, row.freshness),
                ),
            )
            .await;

        let Some(expected) = row.served else {
            assert_eq!(
                status, 400,
                "case {case}: expected a client error, got {status} with body {response}"
            );
            assert_eq!(
                error_code(&response),
                VALIDATION,
                "case {case}: expected {VALIDATION}, got {response}"
            );
            let message = error_message(&response);
            assert_eq!(
                message, FRESHNESS_OUT_OF_RANGE,
                "case {case}: expected the message {FRESHNESS_OUT_OF_RANGE:?}, got {message:?}"
            );

            // The refusal is taken before anything is persisted, so the name it asked for
            // holds no channel ...
            let (read_status, described) = emulator
                .call(
                    "DescribeChannel",
                    &json!({ "ChannelARN": channel_arn(REGION, &name) }),
                )
                .await;
            assert_eq!(
                read_status, 400,
                "case {case}: expected the refused create to have stored no channel, got \
                 {read_status} with body {described}"
            );
            assert_eq!(
                error_code(&described),
                "ResourceNotFoundException",
                "case {case}: expected the refused create to have stored no channel, got \
                 {described}"
            );

            // ... and S3 Tables holds no table for the destination it named.
            let (table_status, body) = emulator.get_table(&bucket_arn, "events", &table).await;
            assert_eq!(
                table_status, 404,
                "case {case}: expected the refused create to have provisioned no table {table}, \
                 got {table_status} with body {body}"
            );
            continue;
        };

        assert_eq!(
            status, 200,
            "case {case}: expected the window inside the range to be served, got {status} with \
             body {response}"
        );
        let reported = response["ChannelDescription"]["S3TablesDestinationConfiguration"]
            ["DataFreshnessInSeconds"]
            .as_i64();
        assert_eq!(
            reported,
            Some(expected),
            "case {case}: expected the create to report the window {expected}, got {response}"
        );

        // What the create accepted is what the STORED channel reports, and its table is there.
        let arn = channel_arn(REGION, &name);
        poll_until_active(&emulator, case, &arn).await;
        assert_eq!(
            reported_freshness(&emulator, case, &arn).await,
            Some(expected),
            "case {case}: expected the stored window {expected} to be read back"
        );
        let (table_status, body) = emulator.get_table(&bucket_arn, "events", &table).await;
        assert_eq!(
            table_status, 200,
            "case {case}: expected the served create to have provisioned its table {table}, got \
             {table_status} with body {body}"
        );
        channels_served.push(name);
    }

    // Exactly the served creates left a channel on the source stream: a refused one leaves no
    // residue on the listing either.
    let case = "the source stream's listing";
    let listed = emulator
        .ok(
            case,
            "ListChannels",
            &json!({ "StreamFilter": [{ "StreamARN": source }] }),
        )
        .await;
    let mut listed_names: Vec<String> = listed["ChannelSummaries"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: ListChannels answers ChannelSummaries, got {listed}"))
        .iter()
        .map(|summary| summary["ChannelName"].as_str().unwrap_or_default().to_string())
        .collect();
    listed_names.sort();
    let mut expected_names = channels_served.clone();
    expected_names.sort();
    assert_eq!(
        listed_names, expected_names,
        "case {case}: expected only the served channels on the source stream, got {listed_names:?}"
    );
}

/// One row of the update table: the window the update names and whether it must be served.
struct FreshnessUpdate {
    case: &'static str,
    freshness: i64,
    served: bool,
}

/// An update moves a streaming table destination's window only inside that same range, and a
/// refused one leaves the stored window as it was and arms no transition.
#[tokio::test]
async fn an_update_moves_a_streaming_table_window_only_inside_the_documented_range() {
    let emulator = Emulator::new();
    let bucket_arn = emulator
        .table_bucket("update-freshness", "ch-tables-update-freshness")
        .await;
    emulator
        .namespace("update-freshness", &bucket_arn, "events")
        .await;
    let source = emulator
        .on_demand_stream("update-freshness", "update-freshness-source")
        .await;

    let case = "the channel every update is sent against";
    let name = "update-freshness-channel";
    let arn = channel_arn(REGION, name);
    emulator
        .ok(
            case,
            "CreateChannel",
            &create_channel_body(
                name,
                &source,
                "GSR_JSON",
                tables_destination_with_freshness(
                    &bucket_arn,
                    "events",
                    "update_freshness_records",
                    Some(600),
                ),
            ),
        )
        .await;
    poll_until_active(&emulator, case, &arn).await;

    let rows = [
        FreshnessUpdate {
            case: "an update one second under the minimum",
            freshness: MIN_FRESHNESS_SECONDS - 1,
            served: false,
        },
        FreshnessUpdate {
            case: "an update one second over the maximum",
            freshness: MAX_FRESHNESS_SECONDS + 1,
            served: false,
        },
        FreshnessUpdate {
            case: "an update to a zero window",
            freshness: 0,
            served: false,
        },
        FreshnessUpdate {
            case: "an update to the minimum",
            freshness: MIN_FRESHNESS_SECONDS,
            served: true,
        },
        FreshnessUpdate {
            case: "an update to the maximum",
            freshness: MAX_FRESHNESS_SECONDS,
            served: true,
        },
        FreshnessUpdate {
            case: "an update inside the range",
            freshness: 777,
            served: true,
        },
    ];

    let mut held = reported_freshness(&emulator, case, &arn).await;
    for row in &rows {
        let case = row.case;
        let (status, response) = emulator
            .call(
                "UpdateChannel",
                &json!({
                    "ChannelARN": arn,
                    "S3TablesDestinationConfiguration": {
                        "DataFreshnessInSeconds": row.freshness,
                    },
                }),
            )
            .await;

        if row.served {
            assert_eq!(
                status, 200,
                "case {case}: expected the window inside the range to be served, got {status} \
                 with body {response}"
            );
            poll_until_active(&emulator, case, &arn).await;
            held = Some(row.freshness);
            assert_eq!(
                reported_freshness(&emulator, case, &arn).await,
                held,
                "case {case}: expected the stored window to be {held:?}"
            );
            continue;
        }

        assert_eq!(
            status, 400,
            "case {case}: expected a client error, got {status} with body {response}"
        );
        assert_eq!(
            error_code(&response),
            VALIDATION,
            "case {case}: expected {VALIDATION}, got {response}"
        );
        let message = error_message(&response);
        assert_eq!(
            message, FRESHNESS_OUT_OF_RANGE,
            "case {case}: expected the message {FRESHNESS_OUT_OF_RANGE:?}, got {message:?}"
        );

        // A refused update writes nothing and arms no transition, so the channel is still
        // ACTIVE on the window it already carried.
        let described = emulator
            .ok(case, "DescribeChannel", &json!({ "ChannelARN": arn }))
            .await;
        let status_reported = described["ChannelDescription"]["ChannelStatus"].as_str();
        assert_eq!(
            status_reported,
            Some(CHANNEL_ACTIVE),
            "case {case}: expected a refused update to leave the channel {CHANNEL_ACTIVE}, got \
             {described}"
        );
        assert_eq!(
            described["ChannelDescription"]["S3TablesDestinationConfiguration"]
                ["DataFreshnessInSeconds"]
                .as_i64(),
            held,
            "case {case}: expected the stored window {held:?} to be left as it was, got \
             {described}"
        );
    }
}
