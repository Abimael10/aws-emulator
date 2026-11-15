// SPDX-License-Identifier: CC0-1.0

//! An event bus whose `LogConfig` asks for logging WRITES records about the events it
//! ingests into the CloudWatch Logs log group a vended-log delivery points it at, end to end
//! over the wire (`awsJson1_1`, `POST /` with `X-Amz-Target: <prefix>.<Operation>`) through
//! the real `MultiServiceRouter`, with CloudWatch Logs mounted on the same engine so the
//! records are asserted where a client reads them: the log events `FilterLogEvents` reports.
//!
//! The RULE asserted here: a bus whose `LogConfig$Level` covers the informational records
//! writes ONE `EVENT_INGEST_SUCCESS` record per event it ingests into the log group of every
//! delivery whose delivery source represents that bus for the log type carrying those
//! records, naming the bus (its ARN and its name), the level the record was written at, the
//! `EventId` the publishing call was answered with and the `details` of the ingestion — the
//! calling account, the event's `Source` and `DetailType`, and the event's own `Detail` only
//! where `IncludeDetail` is `FULL`. A bus whose level does not cover those records writes
//! none, and a bus no delivery names writes nowhere.
//!
//! The tables span what the rule generalises over rather than one recorded request: every
//! modelled `Level` paired with every modelled `IncludeDetail`, a configuration written by
//! `CreateEventBus` and by `UpdateEventBus`, events differing in `Source`, `DetailType`,
//! the shape and size of `Detail` and how many of them one call publishes, and the pipeline
//! differing in whether it exists at all, which log type its source represents the bus
//! under, and which bus it represents. Every row is a FRESH emulator, so one row's pipeline
//! never carries another row's records.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::state::bus_logs::{
    ERROR_LOGS_LOG_TYPE, EVENT_INGEST_SUCCESS, INCLUDE_DETAIL_FULL, INFO_LOGS_LOG_TYPE,
    LOG_LEVEL_ERROR, LOG_LEVEL_INFO, LOG_LEVEL_OFF, LOG_LEVEL_TRACE,
};
use service_com_amazonaws_eventbridge_awsevents::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for, and the Region every ARN here carries.
const REGION: &str = "eu-west-2";

/// The `IncludeDetail` value that carries no event detail (`INCLUDE_DETAIL_VALUES`).
const INCLUDE_DETAIL_NONE: &str = "NONE";

/// EventBridge and CloudWatch Logs on ONE engine — the shape `core/router/src/lib.rs`
/// mounts them in.
struct Aws {
    router: MultiServiceRouter,
}

fn emulator() -> Aws {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    Aws { router }
}

/// One `awsJson` RPC call against the service whose signing name is `signing_name`.
async fn rpc(
    aws: &Aws,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTEST/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = aws.router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice::<Value>(&payload).unwrap_or_else(|error| {
            panic!(
                "{operation} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(&payload)
            )
        })
    };
    (status, parsed)
}

/// One EventBridge call, asserting a 200 and answering the body.
async fn events(aws: &Aws, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answer) = rpc(aws, "events", "AWSEvents", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case}: {operation} answered {status} {answer}"
    );
    answer
}

/// One CloudWatch Logs call, asserting a 200 and answering the body.
async fn logs(aws: &Aws, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answer) = rpc(aws, "logs", "Logs_20140328", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case}: {operation} answered {status} {answer}"
    );
    answer
}

/// A bus called `name`, configured with `log_config` — through `CreateEventBus` when
/// `via_update` is false and through `UpdateEventBus` when it is, because both calls write
/// the same configuration. Answers the bus's ARN, and asserts the configuration reads back.
async fn given_bus(aws: &Aws, case: &str, name: &str, log_config: Option<Value>, via_update: bool) -> String {
    let mut create = json!({ "Name": name });
    if let Some(config) = log_config.clone() {
        if via_update {
            events(aws, case, "CreateEventBus", &create).await;
            events(
                aws,
                case,
                "UpdateEventBus",
                &json!({ "Name": name, "LogConfig": config }),
            )
            .await;
        } else {
            create["LogConfig"] = config;
            events(aws, case, "CreateEventBus", &create).await;
        }
    } else {
        events(aws, case, "CreateEventBus", &create).await;
    }
    let described = events(aws, case, "DescribeEventBus", &json!({ "Name": name })).await;
    let reported = described.get("LogConfig").cloned().unwrap_or(Value::Null);
    let expected = log_config.unwrap_or(Value::Null);
    assert_eq!(
        reported, expected,
        "case {case}: expected DescribeEventBus to report the configuration {expected}, got \
         {reported}"
    );
    described["Arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: DescribeEventBus reported no ARN: {described}"))
        .to_string()
}

/// A log group called `name`, answered with the ARN `DescribeLogGroups` reports for it.
async fn given_log_group(aws: &Aws, case: &str, name: &str) -> String {
    logs(aws, case, "CreateLogGroup", &json!({ "logGroupName": name })).await;
    let listed = logs(
        aws,
        case,
        "DescribeLogGroups",
        &json!({ "logGroupNamePrefix": name }),
    )
    .await;
    listed["logGroups"][0]["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: DescribeLogGroups reported no ARN: {listed}"))
        .to_string()
}

/// The three calls a client builds a vended-log pipeline with: a delivery source over
/// `resource_arn` for `log_type`, a delivery destination over `group_arn`, and the delivery
/// pairing them.
async fn given_delivery(
    aws: &Aws,
    case: &str,
    source_name: &str,
    log_type: &str,
    resource_arn: &str,
    group_arn: &str,
) {
    logs(
        aws,
        case,
        "PutDeliverySource",
        &json!({ "name": source_name, "resourceArn": resource_arn, "logType": log_type }),
    )
    .await;
    let destination = logs(
        aws,
        case,
        "PutDeliveryDestination",
        &json!({
            "name": format!("{source_name}-to-logs"),
            "deliveryDestinationConfiguration": { "destinationResourceArn": group_arn },
        }),
    )
    .await;
    let destination_arn = destination["deliveryDestination"]["arn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case}: PutDeliveryDestination reported no ARN: {destination}")
        })
        .to_string();
    logs(
        aws,
        case,
        "CreateDelivery",
        &json!({ "deliverySourceName": source_name, "deliveryDestinationArn": destination_arn }),
    )
    .await;
}

/// One entry of a `PutEvents` request.
#[derive(Clone)]
struct Entry {
    source: &'static str,
    detail_type: &'static str,
    detail: String,
}

/// Publish `entries` onto `bus` and answer the `EventId` of each, in the request's order.
async fn publish(aws: &Aws, case: &str, bus: &str, entries: &[Entry]) -> Vec<String> {
    let payload: Vec<Value> = entries
        .iter()
        .map(|entry| {
            json!({
                "EventBusName": bus,
                "Source": entry.source,
                "DetailType": entry.detail_type,
                "Detail": entry.detail,
            })
        })
        .collect();
    let answer = events(aws, case, "PutEvents", &json!({ "Entries": payload })).await;
    assert_eq!(
        answer["FailedEntryCount"],
        json!(0),
        "case {case}: expected every entry accepted, got {answer}"
    );
    answer["Entries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|entry| {
            entry["EventId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case}: an accepted entry carries an EventId: {entry}"))
                .to_string()
        })
        .collect()
}

/// Every record the group `group` holds, parsed out of the log events `FilterLogEvents`
/// reports.
async fn records(aws: &Aws, case: &str, group: &str) -> Vec<Value> {
    let answer = logs(
        aws,
        case,
        "FilterLogEvents",
        &json!({ "logGroupName": group }),
    )
    .await;
    answer["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|event| {
            let message = event["message"].as_str().unwrap_or_default();
            serde_json::from_str::<Value>(message).unwrap_or_else(|error| {
                panic!("case {case}: a delivered record is JSON ({error}): {message}")
            })
        })
        .collect()
}

/// The streams the group `group` holds, as `DescribeLogStreams` reports them.
async fn streams(aws: &Aws, case: &str, group: &str) -> Vec<String> {
    let answer = logs(
        aws,
        case,
        "DescribeLogStreams",
        &json!({ "logGroupName": group }),
    )
    .await;
    answer["logStreams"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|stream| stream["logStreamName"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// A `LogConfig` as a client writes one: both members, which is the only form EventBridge
/// accepts.
fn log_config(level: &str, include_detail: &str) -> Value {
    json!({ "Level": level, "IncludeDetail": include_detail })
}

/// The account every resource below belongs to, which is the account the publishing calls
/// are made by.
fn account() -> &'static str {
    engine::emulator_account_id()
}

/// The entries every case publishes: they differ in `Source`, in `DetailType` and in the
/// shape and size of `Detail`, so one row covers a set of events rather than one.
fn entries() -> Vec<Entry> {
    vec![
        Entry {
            source: "custom.orders",
            detail_type: "OrderPlaced",
            detail: json!({ "id": 1, "phase": "post", "qty": 2 }).to_string(),
        },
        Entry {
            source: "com.example.inventory",
            detail_type: "Stock Level Changed",
            detail: json!({ "nested": { "sku": "A-1", "levels": [1, 2, 3] } }).to_string(),
        },
        Entry {
            source: "custom.orders",
            detail_type: "OrderPlaced",
            // An empty detail document, and one carrying the framing of the record that
            // will hold it: a quote, a backslash and a line break.
            detail: json!({ "note": "a \"quoted\"\\value\nover two lines", "empty": {} })
                .to_string(),
        },
    ]
}

/// RULE: a bus whose level covers the informational records writes one
/// `EVENT_INGEST_SUCCESS` record per ingested event into its delivery's log group, carrying
/// the event's own detail only under `IncludeDetail` `FULL`; a bus whose level does not
/// cover them writes nothing.
#[tokio::test]
async fn the_configured_level_and_detail_decide_the_records_a_bus_writes() {
    // One row per (`Level`, `IncludeDetail`) pair over every value the two modelled enums
    // admit (`LEVEL_VALUES`, `INCLUDE_DETAIL_VALUES`), plus a bus that configures no
    // logging at all. `records` is whether an ingest record is written, `detail` whether it
    // carries the event's own `Detail`.
    struct Row {
        case: &'static str,
        config: Option<Value>,
        /// Written through `UpdateEventBus` rather than `CreateEventBus`.
        via_update: bool,
        writes_records: bool,
        carries_detail: bool,
    }
    let rows = vec![
        Row {
            case: "no logging configured at all",
            config: None,
            via_update: false,
            writes_records: false,
            carries_detail: false,
        },
        Row {
            case: "OFF with full detail",
            config: Some(log_config(LOG_LEVEL_OFF, INCLUDE_DETAIL_FULL)),
            via_update: false,
            writes_records: false,
            carries_detail: false,
        },
        Row {
            case: "ERROR with full detail",
            config: Some(log_config(LOG_LEVEL_ERROR, INCLUDE_DETAIL_FULL)),
            via_update: true,
            writes_records: false,
            carries_detail: false,
        },
        Row {
            case: "INFO with no detail",
            config: Some(log_config(LOG_LEVEL_INFO, INCLUDE_DETAIL_NONE)),
            via_update: false,
            writes_records: true,
            carries_detail: false,
        },
        Row {
            case: "INFO with full detail",
            config: Some(log_config(LOG_LEVEL_INFO, INCLUDE_DETAIL_FULL)),
            via_update: true,
            writes_records: true,
            carries_detail: true,
        },
        Row {
            case: "TRACE with no detail",
            config: Some(log_config(LOG_LEVEL_TRACE, INCLUDE_DETAIL_NONE)),
            via_update: true,
            writes_records: true,
            carries_detail: false,
        },
        Row {
            case: "TRACE with full detail",
            config: Some(log_config(LOG_LEVEL_TRACE, INCLUDE_DETAIL_FULL)),
            via_update: false,
            writes_records: true,
            carries_detail: true,
        },
    ];

    for Row {
        case,
        config,
        via_update,
        writes_records,
        carries_detail,
    } in rows
    {
        let aws = emulator();
        let group = "/aws/vendedlogs/events/bus-logs";
        let bus_arn = given_bus(&aws, case, "orders-bus", config, via_update).await;
        let group_arn = given_log_group(&aws, case, group).await;
        given_delivery(
            &aws,
            case,
            "bus-info-logs",
            INFO_LOGS_LOG_TYPE,
            &bus_arn,
            &group_arn,
        )
        .await;

        let published = entries();
        let event_ids = publish(&aws, case, "orders-bus", &published).await;
        let written = records(&aws, case, group).await;

        if !writes_records {
            assert!(
                written.is_empty(),
                "case {case}: expected no record at all, got {written:?}"
            );
            continue;
        }
        assert_eq!(
            written.len(),
            published.len(),
            "case {case}: expected one record per ingested event ({}), got {written:?}",
            published.len()
        );
        for (entry, event_id) in published.iter().zip(event_ids.iter()) {
            let record = written
                .iter()
                .find(|record| record["event_id"] == json!(event_id))
                .unwrap_or_else(|| {
                    panic!("case {case}: no record for EventId {event_id}, got {written:?}")
                });
            for (member, expected) in [
                ("message_type", json!(EVENT_INGEST_SUCCESS)),
                ("resource_arn", json!(bus_arn)),
                ("event_bus_name", json!("orders-bus")),
                ("log_level", json!(LOG_LEVEL_INFO)),
            ] {
                assert_eq!(
                    record[member], expected,
                    "case {case}: expected the {member} member to be {expected}, got {record}"
                );
            }
            assert!(
                record["message_timestamp_ms"].is_i64()
                    && record["message_timestamp_ms"].as_i64().unwrap_or_default() > 0,
                "case {case}: expected an epoch-millisecond instant, got {record}"
            );
            for (member, expected) in [
                ("caller_account_id", json!(account())),
                ("source", json!(entry.source)),
                ("detail_type", json!(entry.detail_type)),
            ] {
                assert_eq!(
                    record["details"][member], expected,
                    "case {case}: expected the {member} detail to be {expected}, got {record}"
                );
            }
            let carried = record["details"].get("event_detail").cloned();
            if carries_detail {
                let carried = carried.unwrap_or(Value::Null);
                assert_eq!(
                    carried,
                    json!(entry.detail),
                    "case {case}: expected the published Detail round-tripped, got {carried}"
                );
                let reparsed: Value = serde_json::from_str(carried.as_str().unwrap_or_default())
                    .unwrap_or_else(|error| {
                        panic!("case {case}: the carried detail is JSON ({error}): {carried}")
                    });
                let published_detail: Value = serde_json::from_str(&entry.detail)
                    .expect("the published detail is JSON");
                assert_eq!(
                    reparsed, published_detail,
                    "case {case}: expected the carried detail to read back as published, got \
                     {reparsed}"
                );
            } else {
                assert!(
                    carried.is_none(),
                    "case {case}: expected no event_detail, got {carried:?}"
                );
            }
        }
    }
}

/// RULE: the records reach a log group only through a delivery that carries THAT bus's logs
/// of that level — before the delivery exists the group holds no stream at all, a group no
/// delivery names stays empty, and a source representing another bus or only the error
/// records carries no ingest record.
#[tokio::test]
async fn only_a_delivery_that_carries_this_buss_informational_logs_receives_the_records() {
    // One row per pipeline the account built, over what the resolution generalises along:
    // whether a delivery exists at all, which log type the source represents the bus under,
    // and which bus it represents.
    struct Row {
        case: &'static str,
        /// The source's `logType`, and `None` for an account that created no pipeline.
        log_type: Option<&'static str>,
        /// The bus the delivery source represents.
        represents_published_bus: bool,
        receives_records: bool,
    }
    let rows = vec![
        Row {
            case: "no delivery at all",
            log_type: None,
            represents_published_bus: true,
            receives_records: false,
        },
        Row {
            case: "a delivery of this bus's informational logs",
            log_type: Some(INFO_LOGS_LOG_TYPE),
            represents_published_bus: true,
            receives_records: true,
        },
        Row {
            case: "a delivery of this bus's error logs alone",
            log_type: Some(ERROR_LOGS_LOG_TYPE),
            represents_published_bus: true,
            receives_records: false,
        },
        Row {
            case: "a delivery of another bus's informational logs",
            log_type: Some(INFO_LOGS_LOG_TYPE),
            represents_published_bus: false,
            receives_records: false,
        },
    ];

    for Row {
        case,
        log_type,
        represents_published_bus,
        receives_records,
    } in rows
    {
        let aws = emulator();
        let group = "/aws/vendedlogs/events/pipeline";
        let published_bus = given_bus(
            &aws,
            case,
            "orders-bus",
            Some(log_config(LOG_LEVEL_INFO, INCLUDE_DETAIL_FULL)),
            false,
        )
        .await;
        let other_bus = given_bus(
            &aws,
            case,
            "audit-bus",
            Some(log_config(LOG_LEVEL_INFO, INCLUDE_DETAIL_FULL)),
            false,
        )
        .await;
        let group_arn = given_log_group(&aws, case, group).await;

        // Before any delivery exists the group holds nothing at all — not even a stream.
        publish(&aws, case, "orders-bus", &entries()).await;
        let before = streams(&aws, case, group).await;
        assert!(
            before.is_empty(),
            "case {case}: expected no stream before a delivery exists, got {before:?}"
        );

        if let Some(log_type) = log_type {
            let represented = if represents_published_bus {
                &published_bus
            } else {
                &other_bus
            };
            given_delivery(&aws, case, "bus-logs", log_type, represented, &group_arn).await;
        }

        let event_ids = publish(&aws, case, "orders-bus", &entries()).await;
        let written = records(&aws, case, group).await;
        if receives_records {
            let carried: Vec<Value> = written
                .iter()
                .filter(|record| record["message_type"] == json!(EVENT_INGEST_SUCCESS))
                .cloned()
                .collect();
            assert_eq!(
                carried.len(),
                event_ids.len(),
                "case {case}: expected one record per event published after the delivery \
                 existed, got {written:?}"
            );
            for event_id in &event_ids {
                assert!(
                    carried
                        .iter()
                        .any(|record| record["event_id"] == json!(event_id)),
                    "case {case}: no record for EventId {event_id}, got {carried:?}"
                );
            }
        } else {
            assert!(
                written.is_empty(),
                "case {case}: expected the group to stay empty, got {written:?}"
            );
        }
    }
}
