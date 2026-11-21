// SPDX-License-Identifier: CC0-1.0

//! What a dynamic-partitioning destination declaring a `RecordDeAggregation` processor
//! delivers.
//!
//! The rule under test: a destination whose dynamic partitioning is enabled and whose
//! enabled data processing declares a `RecordDeAggregation` processor splits each accepted
//! record into the sub-records its `SubRecordType` parameter names — the JSON values the
//! record's bytes carry one after another, or the pieces its base64-encoded `Delimiter`
//! separates — BEFORE the partition keys are derived, so each sub-record is partitioned by
//! the `MetadataExtractionQuery` on its own and lands under its own expanded `Prefix`, while
//! a sub-record the query derived no keys for goes to the `ErrorOutputPrefix`. A destination
//! that declares no such processor keeps every record whole, which is the control row here.
//!
//! Beside that table, one case where the destination ALSO declares a `Lambda` processor: the
//! transformation is handed each sub-record on its own, which is what pins a sub-record having
//! an identity of its own — the entries of a transformation request are told apart by the
//! record id each carries.
//!
//! Driven over the wire on the shared harness of the parent suite: the Firehose calls as
//! `POST /` with `X-Amz-Target`, the destination read as the path-style Amazon S3 requests a
//! client sends.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::delivery_stream_lifecycle::ROLE_ARN;
use crate::destination_delivery::Emulator;
// The zip deployment package of the function the transformation case below names, packaged by
// the sibling transformation suite's own helper rather than a second copy of it.
use crate::lambda_transform_delivery::package;

/// How long the destination is given to hold every object a case expects, and the same
/// generous room for the clock-owed statuses a case waits on. It bounds nothing a case
/// measures, it only stops a wait that will never end.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(60);

/// How often the destination is polled while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The `error-output-type` a record whose partition keys could not be derived is written
/// under, which is the segment the destination's `ErrorOutputPrefix` expands to here.
const METADATA_EXTRACTION_FAILED: &str = "metadata-extraction-failed";

/// One de-aggregation case: what the destination's processing declares, the one record the
/// producer puts, and where its sub-records must land.
struct DeAggregationRow {
    case: &'static str,
    /// The `Processors` of the destination's `ProcessingConfiguration`.
    processors: Value,
    /// The one record `PutRecord` carries, several sub-records packed into it.
    record: &'static str,
    /// What each partition directory under this case's leg must hold: the sub-record texts
    /// the object under it carries, in the order it concatenates them.
    delivered: Vec<(&'static str, Vec<&'static str>)>,
    /// The sub-record texts the error output must carry, each as the `rawData` of one of its
    /// documents. Empty where every sub-record was placed.
    error_output: Vec<&'static str>,
}

/// The jq program every case derives its partition key with, and the engine it is written
/// for: `MetadataExtractionQuery` reads the `tenant` field of each sub-record.
fn metadata_extraction() -> Value {
    json!({
        "Type": "MetadataExtraction",
        "Parameters": [
            { "ParameterName": "MetadataExtractionQuery", "ParameterValue": "{tenant:.tenant}" },
            { "ParameterName": "JsonParsingEngine", "ParameterValue": "JQ-1.6" },
        ],
    })
}

/// A `RecordDeAggregation` processor splitting each record into sub-records of
/// `sub_record_type`, with whatever further parameters that form needs.
fn de_aggregation(sub_record_type: &str, parameters: Value) -> Value {
    let mut declared = vec![json!({
        "ParameterName": "SubRecordType",
        "ParameterValue": sub_record_type,
    })];
    declared.extend(parameters.as_array().cloned().unwrap_or_default());
    json!({ "Type": "RecordDeAggregation", "Parameters": declared })
}

/// The de-aggregation cases.
///
/// The rows differ along what the rule generalises over: which form the sub-records are in
/// (`JSON`, `DELIMITED` — the two values `SubRecordType` carries), how many sub-records one
/// record packs, what sits between them (nothing, whitespace, a delimiter of the producer's
/// choosing), whether two of them share a partition key, and whether the query can derive
/// keys for all of them. The last row is the control: the same aggregated record under a
/// destination that declares no de-aggregation at all.
fn de_aggregation_rows() -> Vec<DeAggregationRow> {
    vec![
        DeAggregationRow {
            case: "four JSON sub-records written one after another",
            processors: json!([de_aggregation("JSON", json!([])), metadata_extraction()]),
            record: "{\"tenant\":\"alpha\"}{\"tenant\":\"bravo\"}{\"tenant\":\"charlie\"}\
                     {\"tenant\":\"delta\"}",
            delivered: vec![
                ("alpha/", vec!["{\"tenant\":\"alpha\"}"]),
                ("bravo/", vec!["{\"tenant\":\"bravo\"}"]),
                ("charlie/", vec!["{\"tenant\":\"charlie\"}"]),
                ("delta/", vec!["{\"tenant\":\"delta\"}"]),
            ],
            error_output: Vec::new(),
        },
        DeAggregationRow {
            case: "JSON sub-records separated by whitespace, two of them sharing a tenant",
            processors: json!([de_aggregation("JSON", json!([])), metadata_extraction()]),
            record: "{\"tenant\":\"alpha\",\"n\":1}\n  {\"tenant\":\"alpha\",\"n\":2}\t\
                     {\"tenant\":\"bravo\",\"n\":3}\n",
            delivered: vec![
                (
                    "alpha/",
                    vec![
                        "{\"tenant\":\"alpha\",\"n\":1}",
                        "{\"tenant\":\"alpha\",\"n\":2}",
                    ],
                ),
                ("bravo/", vec!["{\"tenant\":\"bravo\",\"n\":3}"]),
            ],
            error_output: Vec::new(),
        },
        DeAggregationRow {
            case: "three sub-records a delimiter separates",
            processors: json!([
                // The `Delimiter` parameter is carried base64-encoded, and `fA==` is the one
                // `|` these sub-records are separated by.
                de_aggregation(
                    "DELIMITED",
                    json!([{ "ParameterName": "Delimiter", "ParameterValue": "fA==" }]),
                ),
                metadata_extraction(),
            ]),
            record: "{\"tenant\":\"alpha\"}|{\"tenant\":\"bravo\"}|{\"tenant\":\"charlie\"}",
            delivered: vec![
                ("alpha/", vec!["{\"tenant\":\"alpha\"}"]),
                ("bravo/", vec!["{\"tenant\":\"bravo\"}"]),
                ("charlie/", vec!["{\"tenant\":\"charlie\"}"]),
            ],
            error_output: Vec::new(),
        },
        DeAggregationRow {
            case: "a sub-record the query derives no partition key for",
            processors: json!([de_aggregation("JSON", json!([])), metadata_extraction()]),
            record: "{\"tenant\":\"alpha\"}{\"other\":\"none\"}{\"tenant\":\"bravo\"}",
            delivered: vec![
                ("alpha/", vec!["{\"tenant\":\"alpha\"}"]),
                ("bravo/", vec!["{\"tenant\":\"bravo\"}"]),
            ],
            error_output: vec!["{\"other\":\"none\"}"],
        },
        DeAggregationRow {
            case: "the control: the same record under a destination that de-aggregates nothing",
            processors: json!([metadata_extraction()]),
            record: "{\"tenant\":\"alpha\"}{\"tenant\":\"bravo\"}{\"tenant\":\"charlie\"}\
                     {\"tenant\":\"delta\"}",
            delivered: Vec::new(),
            error_output: vec![
                "{\"tenant\":\"alpha\"}{\"tenant\":\"bravo\"}{\"tenant\":\"charlie\"}\
                 {\"tenant\":\"delta\"}",
            ],
        },
    ]
}

#[tokio::test]
async fn a_de_aggregating_destination_partitions_every_sub_record_of_one_put() {
    let emulator = Emulator::new();
    let bucket = "record-de-aggregation-bucket";
    emulator.create_bucket(bucket).await;

    let rows = de_aggregation_rows();
    let mut legs: Vec<(String, String)> = Vec::new();
    for (ordinal, row) in rows.iter().enumerate() {
        let stream = format!("de-aggregation-{ordinal}");
        let leg = format!("deagg-{ordinal}/");
        emulator
            .firehose_ok(
                "CreateDeliveryStream",
                json!({
                    "DeliveryStreamName": stream,
                    "DeliveryStreamType": "DirectPut",
                    "ExtendedS3DestinationConfiguration": {
                        "RoleARN": ROLE_ARN,
                        "BucketARN": format!("arn:aws:s3:::{bucket}"),
                        "Prefix": format!("{leg}!{{partitionKeyFromQuery:tenant}}/"),
                        "ErrorOutputPrefix": format!("{leg}err/!{{firehose:error-output-type}}/"),
                        "BufferingHints": { "SizeInMBs": 1, "IntervalInSeconds": 0 },
                        "CompressionFormat": "UNCOMPRESSED",
                        "DynamicPartitioningConfiguration": { "Enabled": true },
                        "ProcessingConfiguration": {
                            "Enabled": true,
                            "Processors": row.processors.clone(),
                        },
                    },
                }),
            )
            .await;
        legs.push((stream, leg));
    }
    // A create leaves the stream `CREATING` and the clock settles it, so each stream is
    // polled for the status that takes a put rather than waited on for a fixed time.
    for (stream, _) in &legs {
        await_active(&emulator, stream).await;
    }

    for ((stream, _), row) in legs.iter().zip(&rows) {
        let (status, answered) = emulator.put_record(stream, row.record.as_bytes()).await;
        assert_eq!(
            status, 200,
            "case {:?}: the put must be accepted, got {answered:?}",
            row.case
        );
    }

    for ((_, leg), row) in legs.iter().zip(&rows) {
        let case = row.case;
        let wanted = row.delivered.len() + usize::from(!row.error_output.is_empty());
        // The delivery happens without any further Firehose call, so the destination is
        // polled for the objects the case expects rather than waited on for a fixed time.
        let deadline = Instant::now() + DELIVERY_DEADLINE;
        let keys = loop {
            let keys = emulator.keys_under(bucket, leg).await;
            if keys.len() >= wanted {
                break keys;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected {wanted} object(s) under {leg:?}, got {keys:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        };

        // The partition directories the delivery wrote, which is where each sub-record's own
        // keys placed it.
        let mut directories: Vec<String> = keys
            .iter()
            .filter_map(|key| key.strip_prefix(leg.as_str()))
            .filter_map(|rest| rest.split_once('/').map(|(first, _)| first.to_string()))
            .collect();
        directories.sort();
        directories.dedup();
        let mut expected: Vec<String> = row
            .delivered
            .iter()
            .map(|(directory, _)| directory.trim_end_matches('/').to_string())
            .collect();
        if !row.error_output.is_empty() {
            expected.push("err".to_string());
        }
        expected.sort();
        assert_eq!(
            directories, expected,
            "case {case:?}: expected the sub-records under {expected:?}, got {keys:?}"
        );

        for (directory, pieces) in &row.delivered {
            let under: Vec<String> = keys
                .iter()
                .filter(|key| key.starts_with(&format!("{leg}{directory}")))
                .cloned()
                .collect();
            assert_eq!(
                under.len(),
                1,
                "case {case:?}: one object holds the sub-records of {directory:?}, got {under:?}"
            );
            let body = emulator.object(bucket, &under[0]).await;
            let read = String::from_utf8_lossy(&body).into_owned();
            let want = pieces.concat();
            assert_eq!(
                read, want,
                "case {case:?}: expected {directory:?} to hold {want:?}, got {read:?}"
            );
        }

        let error_keys: Vec<String> = keys
            .iter()
            .filter(|key| key.starts_with(&format!("{leg}err/")))
            .cloned()
            .collect();
        if row.error_output.is_empty() {
            assert!(
                error_keys.is_empty(),
                "case {case:?}: every sub-record carries a partition key, so nothing goes to \
                 the error output, got {error_keys:?}"
            );
            continue;
        }
        assert_eq!(
            error_keys.len(),
            1,
            "case {case:?}: one object holds the error output, got {error_keys:?}"
        );
        assert!(
            error_keys[0].starts_with(&format!("{leg}err/{METADATA_EXTRACTION_FAILED}/")),
            "case {case:?}: the error output names why the record is there, got {:?}",
            error_keys[0]
        );
        let body = emulator.object(bucket, &error_keys[0]).await;
        let parked = raw_data(&body);
        let want: Vec<String> = row
            .error_output
            .iter()
            .map(|piece| (*piece).to_string())
            .collect();
        assert_eq!(
            parked, want,
            "case {case:?}: expected the error output to carry {want:?}, got {parked:?}"
        );
    }
}

/// The `rawData` each document of an error-output object carries, decoded: the records the
/// delivery could not place, as the producer's own bytes.
fn raw_data(body: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(body)
        .lines()
        .map(|line| {
            let document: Value = serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("an error-output document is JSON: {error}"));
            let encoded = document["rawData"]
                .as_str()
                .unwrap_or_else(|| panic!("an error-output document carries rawData: {document}"))
                .to_string();
            let decoded = aws_smithy_types::base64::decode(&encoded)
                .unwrap_or_else(|error| panic!("rawData is base64: {error}"));
            String::from_utf8_lossy(&decoded).into_owned()
        })
        .collect()
}

/// Wait until Firehose reports `stream` `ACTIVE`, which is when it takes a put: the create
/// leaves it `CREATING` and the clock settles it.
async fn await_active(emulator: &Emulator, stream: &str) {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let described = emulator
            .firehose_ok(
                "DescribeDeliveryStream",
                json!({ "DeliveryStreamName": stream }),
            )
            .await;
        if described["DeliveryStreamDescription"]["DeliveryStreamStatus"].as_str()
            == Some("ACTIVE")
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {stream:?}: the stream must become ACTIVE within {DELIVERY_DEADLINE:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

// ---------------------------------------------------------------------------
// De-aggregation beside a data transformation
// ---------------------------------------------------------------------------

/// The function the transformation case below names, and the module and attribute its
/// deployment package carries the handler as.
const FUNCTION: &str = "firehose-sub-record-transform";
const HANDLER: &str = "lambda_function.lambda_handler";

/// The runtime that package is written for.
const RUNTIME: &str = "python3.12";

/// The function's own execution role, satisfying the model's `RoleArn` pattern.
const EXECUTION_ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The handler the transformation case names: it answers `Ok` for the record id it was handed,
/// carrying that record's own data upper-cased, so what reaches the destination says both
/// WHICH record the function judged and that its answer is what was delivered.
const HANDLER_SOURCE: &str = r#"
import base64


def lambda_handler(event, context):
    answered = []
    for record in event["records"]:
        payload = base64.b64decode(record["data"]).decode("utf-8")
        encoded = base64.b64encode(payload.upper().encode("utf-8")).decode("utf-8")
        answered.append(
            {"recordId": record["recordId"], "result": "Ok", "data": encoded}
        )
    return {"records": answered}
"#;

/// One Lambda control-plane call, answering its status and JSON body.
async fn lambda(emulator: &Emulator, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    let payload = body
        .map(|value| serde_json::to_vec(&value).expect("the request body serializes"))
        .unwrap_or_default();
    let answer = emulator.signed("lambda", method, path, &payload).await;
    let parsed = serde_json::from_slice(&answer.body).unwrap_or(Value::Null);
    (answer.status, parsed)
}

/// Create the function the transformation case names, wait until Lambda reports it `Active`,
/// and answer the ARN Lambda minted for it — which is what the processor's `LambdaArn`
/// parameter carries.
async fn given_transform(emulator: &Emulator) -> String {
    let (status, created) = lambda(
        emulator,
        "POST",
        "/2015-03-31/functions",
        Some(json!({
            "FunctionName": FUNCTION,
            "Role": EXECUTION_ROLE,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Code": { "ZipFile": aws_smithy_types::base64::encode(package(HANDLER_SOURCE)) },
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup: CreateFunction must succeed, got {status} {created}"
    );
    let function_arn = created["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: the create must answer a FunctionArn, got {created}"))
        .to_string();
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let (status, reported) = lambda(
            emulator,
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: GetFunctionConfiguration must succeed, got {status} {reported}"
        );
        if reported["State"].as_str() == Some("Active") {
            return function_arn;
        }
        assert!(
            Instant::now() < deadline,
            "setup: Lambda never reported {FUNCTION} Active within {DELIVERY_DEADLINE:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The same rule where the destination ALSO declares a `Lambda` processor: each sub-record is
/// handed to the function on its own and delivered as the verdict for THAT sub-record leaves
/// it, so a destination that de-aggregates and transforms delivers one object per partition
/// key and nothing to the error output.
///
/// Its own test because it needs a function to transform with: the records of one put are
/// told apart by the record id each entry of the transformation request carries, so this is
/// the case that pins a sub-record's identity being its own.
#[tokio::test]
async fn a_de_aggregating_destination_hands_each_sub_record_to_the_data_transformation() {
    let emulator = Emulator::new();
    let bucket = "record-de-aggregation-transform-bucket";
    emulator.create_bucket(bucket).await;
    let function_arn = given_transform(&emulator).await;

    let stream = "de-aggregation-transform";
    let leg = "deagg-transform/";
    emulator
        .firehose_ok(
            "CreateDeliveryStream",
            json!({
                "DeliveryStreamName": stream,
                "DeliveryStreamType": "DirectPut",
                "ExtendedS3DestinationConfiguration": {
                    "RoleARN": ROLE_ARN,
                    "BucketARN": format!("arn:aws:s3:::{bucket}"),
                    "Prefix": format!("{leg}!{{partitionKeyFromQuery:tenant}}/"),
                    "ErrorOutputPrefix": format!("{leg}err/!{{firehose:error-output-type}}/"),
                    "BufferingHints": { "SizeInMBs": 1, "IntervalInSeconds": 0 },
                    "CompressionFormat": "UNCOMPRESSED",
                    "DynamicPartitioningConfiguration": { "Enabled": true },
                    "ProcessingConfiguration": {
                        "Enabled": true,
                        "Processors": [
                            de_aggregation("JSON", json!([])),
                            metadata_extraction(),
                            {
                                "Type": "Lambda",
                                "Parameters": [
                                    {
                                        "ParameterName": "LambdaArn",
                                        "ParameterValue": function_arn,
                                    },
                                ],
                            },
                        ],
                    },
                },
            }),
        )
        .await;
    await_active(&emulator, stream).await;

    let (status, answered) = emulator
        .put_record(
            stream,
            b"{\"tenant\":\"alpha\"}{\"tenant\":\"bravo\"}{\"tenant\":\"charlie\"}",
        )
        .await;
    assert_eq!(
        status, 200,
        "case \"de-aggregation beside a transformation\": the put must be accepted, got \
         {answered:?}"
    );

    let case = "de-aggregation beside a transformation";
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    let keys = loop {
        let keys = emulator.keys_under(bucket, leg).await;
        if keys.len() >= 3 {
            break keys;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected one object per sub-record under {leg:?}, got {keys:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    };
    for (directory, delivered) in [
        ("alpha/", "{\"TENANT\":\"ALPHA\"}"),
        ("bravo/", "{\"TENANT\":\"BRAVO\"}"),
        ("charlie/", "{\"TENANT\":\"CHARLIE\"}"),
    ] {
        let under: Vec<String> = keys
            .iter()
            .filter(|key| key.starts_with(&format!("{leg}{directory}")))
            .cloned()
            .collect();
        assert_eq!(
            under.len(),
            1,
            "case {case:?}: one object holds the sub-record of {directory:?}, got {under:?}"
        );
        let body = emulator.object(bucket, &under[0]).await;
        let read = String::from_utf8_lossy(&body).into_owned();
        assert_eq!(
            read, delivered,
            "case {case:?}: expected {directory:?} to hold what the function answered for \
             that sub-record, got {read:?}"
        );
    }
    let error_keys: Vec<String> = keys
        .iter()
        .filter(|key| key.starts_with(&format!("{leg}err/")))
        .cloned()
        .collect();
    assert!(
        error_keys.is_empty(),
        "case {case:?}: the function placed every sub-record, so nothing goes to the error \
         output, got {error_keys:?}"
    );
}
