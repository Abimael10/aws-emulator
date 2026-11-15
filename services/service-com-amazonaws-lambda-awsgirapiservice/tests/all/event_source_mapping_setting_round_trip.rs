// SPDX-License-Identifier: CC0-1.0

//! What a created event source mapping REPORTS BACK of the settings it was created with, over
//! the wire, through `CreateEventSourceMapping`'s own answer, `GetEventSourceMapping` and
//! `ListEventSourceMappings`.
//!
//! RULE: a reported mapping carries, for every setting of
//! `EventSourceMappingConfiguration`, the value the accepted create was given; where the
//! request stated none it carries the default that member's own documentation gives it for the
//! event source the mapping names; and a member that documentation scopes to OTHER sources is
//! absent from the response rather than reported as a null. So a caller reconciling a desired
//! configuration against the one the mapping reports sees no difference it did not ask for, and
//! a strict deserializer never meets a null where AWS omits the key.
//!
//! The defaults and the per-source scopes are the model's own
//! (`smithy_query(query="shape",
//! shape="com.amazonaws.lambda#EventSourceMappingConfiguration")`):
//!
//! * `BatchSize` — "Default value: Varies by service. For Amazon SQS, the default is 10. For all
//!   other services, the default is 100.";
//! * `MaximumBatchingWindowInSeconds` — "For streams and Amazon SQS event sources, the default
//!   batching window is 0 seconds. For Amazon MSK, Self-managed Apache Kafka, Amazon MQ, and
//!   DocumentDB event sources, the default batching window is 500 ms", a duration this member's
//!   unit (whole seconds) cannot carry, so those sources report none at all;
//! * `ParallelizationFactor` — "(Kinesis and DynamoDB Streams only) … The default value is 1.";
//! * `MaximumRecordAgeInSeconds`, `MaximumRetryAttempts` — "(Kinesis, DynamoDB Streams, Amazon
//!   MSK, and self-managed Apache Kafka) … The default value is -1, which sets the maximum age
//!   to infinite";
//! * `BisectBatchOnFunctionError` — same four sources, "The default value is false";
//! * `TumblingWindowInSeconds` — "(Kinesis and DynamoDB Streams only) … A value of 0 seconds
//!   indicates no tumbling window";
//! * `FunctionResponseTypes` — "a list of current response type enums applied to the event
//!   source mapping", which is the empty list for a mapping none is applied to.
//!
//! The rows differ along the source service (a DynamoDB stream, a Kinesis data stream, a queue
//! and an Amazon MSK cluster — a shard source, a stream source that is not one, and a source
//! neither scope covers), along how much of the configuration the request states (nothing
//! beyond what the source requires, a few settings, every setting the source scopes) and along
//! the values stated (each setting's own published bounds and values between them).

use std::sync::Arc;

use serde_json::{json, Value};

// The reader of a reported mapping's members, and the harness around a create and its two
// readers, are the sibling suite's: one owner for each, so no two tables over this
// operation can drift apart.
use crate::event_source_mappings::{
    assert_members, assert_no_null_member, create_function, create_mapping, create_request,
    get_mapping, list_mappings, router_on, DYNAMODB_SOURCE, KINESIS_SOURCE, MSK_SOURCE,
    QUEUE_SOURCE,
};

/// The one `FunctionResponseType` the model's enum declares.
const REPORT_BATCH_ITEM_FAILURES: &str = "ReportBatchItemFailures";

/// The Kafka topic a mapping onto an Amazon MSK cluster consumes, which such a mapping names
/// beside its cluster.
const KAFKA_TOPIC: &str = "order-events";

/// One row: the source the mapping names, what the create states, and the settings every
/// reader of the mapping must report — where `Value::Null` means the member must be ABSENT
/// from the response.
struct SettingRow {
    case: &'static str,
    event_source: &'static str,
    extras: Value,
    reported: Value,
}

/// The rows. Each `reported` names every setting of the configuration that the source it is
/// written for scopes, plus the ones it does not (expected absent), so a value that travelled
/// into the store and a value that did not are both caught.
fn setting_rows() -> Vec<SettingRow> {
    vec![
        SettingRow {
            case: "a dynamodb stream whose request states the batch size and the retry allowance",
            event_source: DYNAMODB_SOURCE,
            extras: json!({
                "StartingPosition": "LATEST",
                "BatchSize": 1,
                "MaximumRetryAttempts": 3,
            }),
            reported: json!({
                "StartingPosition": "LATEST",
                "BatchSize": 1,
                "MaximumRetryAttempts": 3,
                "MaximumBatchingWindowInSeconds": 0,
                "ParallelizationFactor": 1,
                "MaximumRecordAgeInSeconds": -1,
                "BisectBatchOnFunctionError": false,
                "TumblingWindowInSeconds": 0,
                "FunctionResponseTypes": [],
                "StartingPositionTimestamp": Value::Null,
                "Topics": Value::Null,
                "Queues": Value::Null,
            }),
        },
        SettingRow {
            case: "a dynamodb stream whose request states every setting its source scopes",
            event_source: DYNAMODB_SOURCE,
            extras: json!({
                "StartingPosition": "TRIM_HORIZON",
                // `BatchSize` at its published ceiling (`@range(min: 1, max: 10000)`), with a
                // batching window the same member's "related setting" asks for past 10.
                "BatchSize": 10_000,
                "MaximumBatchingWindowInSeconds": 300,
                "ParallelizationFactor": 10,
                "MaximumRecordAgeInSeconds": 604_800,
                "MaximumRetryAttempts": 10_000,
                "BisectBatchOnFunctionError": true,
                "TumblingWindowInSeconds": 900,
                "FunctionResponseTypes": [REPORT_BATCH_ITEM_FAILURES],
            }),
            reported: json!({
                "StartingPosition": "TRIM_HORIZON",
                "BatchSize": 10_000,
                "MaximumBatchingWindowInSeconds": 300,
                "ParallelizationFactor": 10,
                "MaximumRecordAgeInSeconds": 604_800,
                "MaximumRetryAttempts": 10_000,
                "BisectBatchOnFunctionError": true,
                "TumblingWindowInSeconds": 900,
                "FunctionResponseTypes": [REPORT_BATCH_ITEM_FAILURES],
            }),
        },
        SettingRow {
            case: "a dynamodb stream whose request states the settings at their floors",
            event_source: DYNAMODB_SOURCE,
            extras: json!({
                "StartingPosition": "LATEST",
                "BatchSize": 1,
                "MaximumBatchingWindowInSeconds": 0,
                "ParallelizationFactor": 1,
                "MaximumRecordAgeInSeconds": -1,
                "MaximumRetryAttempts": -1,
                "BisectBatchOnFunctionError": false,
                "TumblingWindowInSeconds": 0,
                "FunctionResponseTypes": [],
            }),
            reported: json!({
                "StartingPosition": "LATEST",
                "BatchSize": 1,
                "MaximumBatchingWindowInSeconds": 0,
                "ParallelizationFactor": 1,
                "MaximumRecordAgeInSeconds": -1,
                "MaximumRetryAttempts": -1,
                "BisectBatchOnFunctionError": false,
                "TumblingWindowInSeconds": 0,
                "FunctionResponseTypes": [],
            }),
        },
        SettingRow {
            case: "a kinesis data stream whose request states nothing beyond the position",
            event_source: KINESIS_SOURCE,
            extras: json!({ "StartingPosition": "LATEST" }),
            reported: json!({
                "StartingPosition": "LATEST",
                "BatchSize": 100,
                "MaximumBatchingWindowInSeconds": 0,
                "ParallelizationFactor": 1,
                "MaximumRecordAgeInSeconds": -1,
                "MaximumRetryAttempts": -1,
                "BisectBatchOnFunctionError": false,
                "TumblingWindowInSeconds": 0,
                "FunctionResponseTypes": [],
            }),
        },
        SettingRow {
            case: "a kinesis data stream whose request states a window and a record age",
            event_source: KINESIS_SOURCE,
            extras: json!({
                "StartingPosition": "TRIM_HORIZON",
                "BatchSize": 42,
                "MaximumBatchingWindowInSeconds": 7,
                "MaximumRecordAgeInSeconds": 3_600,
                "TumblingWindowInSeconds": 450,
            }),
            reported: json!({
                "StartingPosition": "TRIM_HORIZON",
                "BatchSize": 42,
                "MaximumBatchingWindowInSeconds": 7,
                "MaximumRecordAgeInSeconds": 3_600,
                "TumblingWindowInSeconds": 450,
                // Stated by neither the request nor the source's requirements, so each is the
                // default its own documentation gives it.
                "ParallelizationFactor": 1,
                "MaximumRetryAttempts": -1,
                "BisectBatchOnFunctionError": false,
                "FunctionResponseTypes": [],
            }),
        },
        SettingRow {
            case: "a queue whose request states no setting at all",
            event_source: QUEUE_SOURCE,
            extras: json!({}),
            reported: json!({
                "BatchSize": 10,
                "MaximumBatchingWindowInSeconds": 0,
                "FunctionResponseTypes": [],
                // A queue is neither a shard source nor one of the four stream sources the
                // error-handling options are scoped to, so none of those members applies.
                "StartingPosition": Value::Null,
                "ParallelizationFactor": Value::Null,
                "TumblingWindowInSeconds": Value::Null,
                "MaximumRecordAgeInSeconds": Value::Null,
                "MaximumRetryAttempts": Value::Null,
                "BisectBatchOnFunctionError": Value::Null,
            }),
        },
        SettingRow {
            case: "a queue whose request states the batch size and the response types",
            event_source: QUEUE_SOURCE,
            extras: json!({
                "BatchSize": 5,
                "MaximumBatchingWindowInSeconds": 30,
                "FunctionResponseTypes": [REPORT_BATCH_ITEM_FAILURES],
            }),
            reported: json!({
                "BatchSize": 5,
                "MaximumBatchingWindowInSeconds": 30,
                "FunctionResponseTypes": [REPORT_BATCH_ITEM_FAILURES],
                "StartingPosition": Value::Null,
                "ParallelizationFactor": Value::Null,
                "TumblingWindowInSeconds": Value::Null,
                "MaximumRecordAgeInSeconds": Value::Null,
                "MaximumRetryAttempts": Value::Null,
                "BisectBatchOnFunctionError": Value::Null,
            }),
        },
        SettingRow {
            case: "an msk cluster, a stream source whose default batching window has no value \
                   in this member's unit",
            event_source: MSK_SOURCE,
            extras: json!({ "Topics": [KAFKA_TOPIC] }),
            reported: json!({
                "Topics": [KAFKA_TOPIC],
                "BatchSize": 100,
                // One of the four stream sources, so the error-handling options apply and
                // carry their documented defaults.
                "MaximumRecordAgeInSeconds": -1,
                "MaximumRetryAttempts": -1,
                "BisectBatchOnFunctionError": false,
                "FunctionResponseTypes": [],
                // Not a shard source, and a 500 ms default window is not a whole number of
                // seconds.
                "MaximumBatchingWindowInSeconds": Value::Null,
                "ParallelizationFactor": Value::Null,
                "TumblingWindowInSeconds": Value::Null,
            }),
        },
        SettingRow {
            case: "an msk cluster whose request states the error-handling options",
            event_source: MSK_SOURCE,
            extras: json!({
                "Topics": [KAFKA_TOPIC],
                "BatchSize": 2,
                "MaximumBatchingWindowInSeconds": 1,
                "MaximumRecordAgeInSeconds": 120,
                "MaximumRetryAttempts": 4,
                "BisectBatchOnFunctionError": true,
            }),
            reported: json!({
                "Topics": [KAFKA_TOPIC],
                "BatchSize": 2,
                "MaximumBatchingWindowInSeconds": 1,
                "MaximumRecordAgeInSeconds": 120,
                "MaximumRetryAttempts": 4,
                "BisectBatchOnFunctionError": true,
                "FunctionResponseTypes": [],
                "ParallelizationFactor": Value::Null,
                "TumblingWindowInSeconds": Value::Null,
            }),
        },
    ]
}

/// RULE: every reader of a mapping reports the settings the accepted create was given, the
/// documented default of each setting the request left out for the source the mapping names,
/// and no member the source's documentation does not scope to it — omitted, never null.
///
/// Each row is read three ways, so a value the create answered but never stored, and a value
/// one reader builds differently from another, both fail here.
#[tokio::test]
async fn a_created_mapping_reports_the_settings_it_was_created_with() {
    // Lambda alone: every reading here is of what the control plane ACCEPTED and REPORTS, so
    // no stream, queue or cluster has to exist for it.
    let router = router_on(&Arc::new(engine::InMemoryEngine::new()));
    for (index, row) in setting_rows().iter().enumerate() {
        let case = row.case;
        let function = format!("esm-round-trip-{index}");
        create_function(&router, &function).await;

        let created = create_mapping(
            &router,
            &create_request(&function, Some(row.event_source), &row.extras),
        )
        .await;
        assert_eq!(
            created.status,
            202,
            "case {case:?}: CreateEventSourceMapping must answer 202 Accepted, got {} {}",
            created.status,
            created.text()
        );
        assert_members(case, "the create", &created.json(), &row.reported);
        assert_no_null_member(case, "the create", &created.json());

        let uuid = created.string("UUID").unwrap_or_else(|| {
            panic!(
                "case {case:?}: the create answers a UUID, got {}",
                created.text()
            )
        });

        let fetched = get_mapping(&router, &uuid).await;
        assert_eq!(
            fetched.status,
            200,
            "case {case:?}: GetEventSourceMapping must answer 200, got {} {}",
            fetched.status,
            fetched.text()
        );
        assert_members(case, "the get", &fetched.json(), &row.reported);
        assert_no_null_member(case, "the get", &fetched.json());

        let listed = list_mappings(&router, &format!("FunctionName={function}")).await;
        let entry = listed.mappings().first().cloned().unwrap_or_else(|| {
            panic!(
                "case {case:?}: the listing holds the mapping, got {}",
                listed.text()
            )
        });
        assert_members(case, "the listing", &entry, &row.reported);
        assert_no_null_member(case, "the listing", &entry);
    }
}
