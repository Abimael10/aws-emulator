// SPDX-License-Identifier: CC0-1.0

//! The informational records an event bus vends reach EVERY delivery that carries them, and one
//! log group several buses deliver into keeps each sending bus's records apart.
//!
//! RULE: a bus whose `LogConfig$Level` covers the informational records writes one
//! `EVENT_INGEST_SUCCESS` record per event it ingests into the destination of EVERY delivery
//! whose delivery source represents that bus for a log type carrying those records — a vended-log
//! pipeline is a fan-out of the SENDING resource, since `PutDeliverySource` represents "the
//! resource that is actually sending the logs", a `CreateDelivery` is "a delivery [...] pairing
//! exactly one delivery source and one delivery destination", and several deliveries may name one
//! source — while a bus whose level covers them writes nowhere at all. Each record names the bus
//! it is about (its ARN and its name), the `EventId` the publishing call was answered with, the
//! calling account and, under `IncludeDetail` `FULL`, the event's own `Detail`. Because the record
//! is the SENDER's document, the records of one bus are kept together and apart from another
//! bus's: a group that is the destination of deliveries of several buses holds each bus's records
//! in a log stream of its own, and a read across the group's streams (`FilterLogEvents` "lists log
//! events from the specified log group") reports all of them.
//!
//! Only a cross-service suite can pin this: the sending side is an EventBridge bus's log
//! configuration and the delivery side is CloudWatch Logs' own delivery sources, destinations,
//! deliveries and log groups, so the record and the configuration that produced it live in
//! different services of one engine. The EventBridge crate's own `bus_log_delivery` suite pins
//! which configurations write a record and what one record says, over ONE delivery; neither it nor
//! the CloudWatch Logs crate's delivery suites state that one source's records reach every
//! delivery of it, nor that one group keeps two senders' records apart.
//!
//! The tables span what the rule generalises over rather than the one recorded pipeline: how many
//! deliveries one source has (one, two and three, each into a destination of its own), whether the
//! bus's level covers the records at all, how many events one publishing call carries, the
//! `Source`, `DetailType` and the shape of the `Detail` of each, and — for the shared group — how
//! many buses deliver into it and with which `IncludeDetail` each does.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::state::bus_logs::{
    EVENT_INGEST_SUCCESS, INCLUDE_DETAIL_FULL, INFO_LOGS_LOG_TYPE, LOG_LEVEL_INFO, LOG_LEVEL_OFF,
};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for, and the Region every ARN here carries.
const REGION: &str = "eu-central-1";

/// The `IncludeDetail` value that carries no event detail, beside [`INCLUDE_DETAIL_FULL`]
/// (`smithy_query(query="shape",shape="IncludeDetail")`).
const INCLUDE_DETAIL_NONE: &str = "NONE";

/// EventBridge and CloudWatch Logs on ONE engine — the shape `core/router/src/lib.rs` mounts them
/// in — with request authentication off, so each call carries a credential scope (which is what
/// names the Region) without a computed signature.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
    );
    router
}

/// One `awsJson` RPC call against the service whose signing name is `signing_name`.
async fn rpc(
    router: &MultiServiceRouter,
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
    let response = router.dispatch(request).await;
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
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answer) = rpc(router, "events", "AWSEvents", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {operation} to be accepted, got {status} {answer}"
    );
    answer
}

/// One CloudWatch Logs call, asserting a 200 and answering the body.
async fn logs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answer) = rpc(router, "logs", "Logs_20140328", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {operation} to be accepted, got {status} {answer}"
    );
    answer
}

/// A bus called `name`, created with the log configuration `(level, include_detail)`, answered
/// with its ARN. The configuration is read back through `DescribeEventBus`, so a row's pipeline
/// is built over a bus whose stored configuration is the one it was created with.
async fn given_bus(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    level: &str,
    include_detail: &str,
) -> String {
    let configured = json!({ "Level": level, "IncludeDetail": include_detail });
    let created = events(
        router,
        case,
        "CreateEventBus",
        &json!({ "Name": name, "LogConfig": configured }),
    )
    .await;
    let arn = created["EventBusArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateEventBus reported no ARN: {created}"))
        .to_string();
    let described = events(router, case, "DescribeEventBus", &json!({ "Name": name })).await;
    assert_eq!(
        described["LogConfig"], configured,
        "case {case:?}: expected DescribeEventBus to report the configuration {configured}, got \
         {described}"
    );
    assert_eq!(
        described["Arn"],
        json!(arn),
        "case {case:?}: expected both calls to report one ARN, got {described}"
    );
    arn
}

/// A log group called `name`, answered with the ARN `DescribeLogGroups` reports for it.
async fn given_log_group(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    logs(
        router,
        case,
        "CreateLogGroup",
        &json!({ "logGroupName": name }),
    )
    .await;
    let listed = logs(
        router,
        case,
        "DescribeLogGroups",
        &json!({ "logGroupNamePrefix": name }),
    )
    .await;
    listed["logGroups"][0]["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: DescribeLogGroups reported no ARN: {listed}"))
        .to_string()
}

/// A delivery source called `name` representing `resource_arn` for `log_type`.
async fn given_delivery_source(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    log_type: &str,
    resource_arn: &str,
) {
    logs(
        router,
        case,
        "PutDeliverySource",
        &json!({ "name": name, "resourceArn": resource_arn, "logType": log_type }),
    )
    .await;
}

/// A delivery destination called `name` over the log group `group_arn`, answered with its own ARN.
async fn given_delivery_destination(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    group_arn: &str,
) -> String {
    let destination = logs(
        router,
        case,
        "PutDeliveryDestination",
        &json!({
            "name": name,
            "deliveryDestinationConfiguration": { "destinationResourceArn": group_arn },
        }),
    )
    .await;
    destination["deliveryDestination"]["arn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: PutDeliveryDestination reported no ARN: {destination}")
        })
        .to_string()
}

/// The delivery pairing the source `source_name` with the destination `destination_arn`.
async fn given_delivery(
    router: &MultiServiceRouter,
    case: &str,
    source_name: &str,
    destination_arn: &str,
) {
    logs(
        router,
        case,
        "CreateDelivery",
        &json!({
            "deliverySourceName": source_name,
            "deliveryDestinationArn": destination_arn,
        }),
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

/// The entries a case publishes: they differ in `Source`, in `DetailType` and in the shape and
/// size of `Detail`, so one row covers a set of events rather than one. `count` of them are taken,
/// so a row also varies how many events one publishing call carries.
fn entries(count: usize) -> Vec<Entry> {
    let all = vec![
        Entry {
            source: "custom.orders",
            detail_type: "OrderPlaced",
            detail: json!({ "id": 1, "qty": 2 }).to_string(),
        },
        Entry {
            source: "com.example.inventory",
            detail_type: "Stock Level Changed",
            detail: json!({ "nested": { "sku": "A-1", "levels": [1, 2, 3] } }).to_string(),
        },
        Entry {
            source: "custom.orders",
            detail_type: "OrderPlaced",
            // A detail carrying the framing of the record that will hold it: a quote, a
            // backslash and a line break, beside an empty object.
            detail: json!({ "note": "a \"quoted\"\\value\nover two lines", "empty": {} })
                .to_string(),
        },
    ];
    all.into_iter().cycle().take(count).collect()
}

/// Publish `entries` onto `bus` and answer the `EventId` of each, in the request's order.
async fn publish(
    router: &MultiServiceRouter,
    case: &str,
    bus: &str,
    entries: &[Entry],
) -> Vec<String> {
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
    let answer = events(router, case, "PutEvents", &json!({ "Entries": payload })).await;
    assert_eq!(
        answer["FailedEntryCount"],
        json!(0),
        "case {case:?}: expected every entry accepted, got {answer}"
    );
    answer["Entries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|entry| {
            entry["EventId"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: an accepted entry carries an EventId: {entry}")
                })
                .to_string()
        })
        .collect()
}

/// Every record the group `group` holds, as (log stream, parsed record) pairs read out of the log
/// events `FilterLogEvents` reports across the group's streams.
async fn records(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<(String, Value)> {
    let answer = logs(
        router,
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
            let parsed = serde_json::from_str::<Value>(message).unwrap_or_else(|error| {
                panic!("case {case:?}: a delivered record is JSON ({error}): {message}")
            });
            let stream = event["logStreamName"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: a reported log event names its stream: {event}")
                })
                .to_string();
            (stream, parsed)
        })
        .collect()
}

/// The ingest records of `held`, with the stream each was written into.
fn ingest_records(held: &[(String, Value)]) -> Vec<(String, Value)> {
    held.iter()
        .filter(|(_, record)| record["message_type"] == json!(EVENT_INGEST_SUCCESS))
        .cloned()
        .collect()
}

/// The account every resource below belongs to, which is the account the publishing calls are
/// made by.
fn account() -> &'static str {
    engine::emulator_account_id()
}

/// Assert that `held` is exactly one ingest record per `event_ids`, each naming `bus` and
/// `bus_arn`, each carrying the caller's account, and each carrying the event's own detail only
/// when `carries_detail`.
fn assert_one_record_per_event(
    case: &str,
    held: &[(String, Value)],
    event_ids: &[String],
    bus: &str,
    bus_arn: &str,
    published: &[Entry],
    carries_detail: bool,
) {
    let carried = ingest_records(held);
    assert_eq!(
        carried.len(),
        event_ids.len(),
        "case {case:?}: expected one {EVENT_INGEST_SUCCESS} record per ingested event ({}), got \
         {carried:?}",
        event_ids.len()
    );
    for (event_id, entry) in event_ids.iter().zip(published.iter()) {
        let (_, record) = carried
            .iter()
            .find(|(_, record)| record["event_id"] == json!(event_id))
            .unwrap_or_else(|| {
                panic!("case {case:?}: no record for EventId {event_id}, got {carried:?}")
            });
        for (member, expected) in [
            ("resource_arn", json!(bus_arn)),
            ("event_bus_name", json!(bus)),
            ("log_level", json!(LOG_LEVEL_INFO)),
        ] {
            assert_eq!(
                record[member], expected,
                "case {case:?}: expected the {member} member to be {expected}, got {record}"
            );
        }
        for (member, expected) in [
            ("caller_account_id", json!(account())),
            ("source", json!(entry.source)),
            ("detail_type", json!(entry.detail_type)),
        ] {
            assert_eq!(
                record["details"][member], expected,
                "case {case:?}: expected the {member} detail to be {expected}, got {record}"
            );
        }
        let detail = record["details"].get("event_detail").cloned();
        if carries_detail {
            assert_eq!(
                detail.clone().unwrap_or(Value::Null),
                json!(entry.detail),
                "case {case:?}: expected the published Detail round-tripped, got {detail:?}"
            );
        } else {
            assert!(
                detail.is_none(),
                "case {case:?}: expected no event_detail, got {detail:?}"
            );
        }
    }
}

/// RULE: one source's informational records reach the destination of EVERY delivery of that
/// source, once each; a bus whose level covers no informational record reaches none of them.
#[tokio::test]
async fn every_delivery_of_one_source_receives_each_ingest_record() {
    struct Row {
        case: &'static str,
        /// The bus's `LogConfig$Level`.
        level: &'static str,
        /// The bus's `LogConfig$IncludeDetail`.
        include_detail: &'static str,
        /// How many deliveries of the one source exist, each into a log group of its own.
        deliveries: usize,
        /// How many events the publishing call carries.
        events: usize,
    }
    let rows = [
        Row {
            case: "one delivery of the source",
            level: LOG_LEVEL_INFO,
            include_detail: INCLUDE_DETAIL_FULL,
            deliveries: 1,
            events: 1,
        },
        Row {
            case: "two deliveries of one source",
            level: LOG_LEVEL_INFO,
            include_detail: INCLUDE_DETAIL_FULL,
            deliveries: 2,
            events: 3,
        },
        Row {
            case: "three deliveries of one source, detail withheld",
            level: LOG_LEVEL_INFO,
            include_detail: INCLUDE_DETAIL_NONE,
            deliveries: 3,
            events: 2,
        },
        Row {
            case: "three deliveries of a bus whose logging is off",
            level: LOG_LEVEL_OFF,
            include_detail: INCLUDE_DETAIL_FULL,
            deliveries: 3,
            events: 3,
        },
    ];

    for row in rows {
        let case = row.case;
        // A FRESH emulator per row, so one row's pipeline never carries another row's records.
        let router = emulator();
        let bus = "orders-bus";
        let bus_arn = given_bus(&router, case, bus, row.level, row.include_detail).await;

        let source = "orders-bus-info-logs";
        given_delivery_source(&router, case, source, INFO_LOGS_LOG_TYPE, &bus_arn).await;
        let mut groups: Vec<String> = Vec::new();
        for index in 0..row.deliveries {
            let group = format!("/aws/vendedlogs/events/fan-out-{index}");
            let group_arn = given_log_group(&router, case, &group).await;
            let destination_arn =
                given_delivery_destination(&router, case, &format!("to-group-{index}"), &group_arn)
                    .await;
            given_delivery(&router, case, source, &destination_arn).await;
            groups.push(group);
        }

        let published = entries(row.events);
        let event_ids = publish(&router, case, bus, &published).await;

        for group in &groups {
            let held = records(&router, case, group).await;
            if row.level == LOG_LEVEL_OFF {
                assert!(
                    held.is_empty(),
                    "case {case:?}: expected {group} to stay empty, got {held:?}"
                );
                continue;
            }
            assert_one_record_per_event(
                case,
                &held,
                &event_ids,
                bus,
                &bus_arn,
                &published,
                row.include_detail == INCLUDE_DETAIL_FULL,
            );
        }
    }
}

/// RULE: one log group that is the destination of deliveries of several buses holds each bus's
/// records in a stream of its own, a bus whose level covers no informational record contributes
/// none, and a read across the group's streams reports all of them.
#[tokio::test]
async fn one_group_keeps_each_sending_buss_records_in_its_own_stream() {
    struct Row {
        /// The bus the delivery source represents.
        bus: &'static str,
        level: &'static str,
        include_detail: &'static str,
        /// How many events are published onto it.
        events: usize,
    }
    // The senders delivering into ONE group: they differ in their level, in their
    // `IncludeDetail`, and in how many events each ingests.
    let senders = [
        Row {
            bus: "orders-bus",
            level: LOG_LEVEL_INFO,
            include_detail: INCLUDE_DETAIL_FULL,
            events: 2,
        },
        Row {
            bus: "inventory-bus",
            level: LOG_LEVEL_INFO,
            include_detail: INCLUDE_DETAIL_NONE,
            events: 3,
        },
        Row {
            bus: "quiet-bus",
            level: LOG_LEVEL_OFF,
            include_detail: INCLUDE_DETAIL_FULL,
            events: 2,
        },
    ];

    let case = "three buses delivering into one group";
    let router = emulator();
    let group = "/aws/vendedlogs/events/shared";
    let group_arn = given_log_group(&router, case, group).await;
    // ONE destination over the group, paired with each sender's own source, so every delivery
    // writes into the same group.
    let destination_arn =
        given_delivery_destination(&router, case, "to-shared-group", &group_arn).await;

    let mut published: Vec<(&Row, String, Vec<Entry>, Vec<String>)> = Vec::new();
    for sender in &senders {
        let bus_arn = given_bus(&router, case, sender.bus, sender.level, sender.include_detail)
            .await;
        let source = format!("{}-info-logs", sender.bus);
        given_delivery_source(&router, case, &source, INFO_LOGS_LOG_TYPE, &bus_arn).await;
        given_delivery(&router, case, &source, &destination_arn).await;
        published.push((sender, bus_arn, entries(sender.events), Vec::new()));
    }
    for entry in &mut published {
        let (sender, _, events, event_ids) = entry;
        *event_ids = publish(&router, case, sender.bus, events).await;
    }

    let held = records(&router, case, group).await;
    let carried = ingest_records(&held);
    let expected: usize = senders
        .iter()
        .filter(|sender| sender.level != LOG_LEVEL_OFF)
        .map(|sender| sender.events)
        .sum();
    assert_eq!(
        carried.len(),
        expected,
        "case {case:?}: expected the group to hold one record per event of every bus whose level \
         covers them ({expected}), got {carried:?}"
    );

    // Each stream holds the records of ONE bus, and each logging bus has a stream of its own.
    let mut buses_by_stream: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (stream, record) in &carried {
        buses_by_stream
            .entry(stream.clone())
            .or_default()
            .insert(record["event_bus_name"].as_str().unwrap_or_default().to_string());
    }
    for (stream, buses) in &buses_by_stream {
        assert_eq!(
            buses.len(),
            1,
            "case {case:?}: expected stream {stream} to hold one bus's records, got {buses:?}"
        );
    }
    let logging: Vec<&Row> = senders
        .iter()
        .filter(|sender| sender.level != LOG_LEVEL_OFF)
        .collect();
    assert_eq!(
        buses_by_stream.len(),
        logging.len(),
        "case {case:?}: expected one stream per bus whose level covers the records ({}), got {:?}",
        logging.len(),
        buses_by_stream.keys().collect::<Vec<&String>>()
    );

    // And every bus's own records are exactly the ones it owes.
    for (sender, bus_arn, events, event_ids) in &published {
        let own: Vec<(String, Value)> = carried
            .iter()
            .filter(|(_, record)| record["event_bus_name"] == json!(sender.bus))
            .cloned()
            .collect();
        let sender_case = format!("{case}: {}", sender.bus);
        if sender.level == LOG_LEVEL_OFF {
            assert!(
                own.is_empty(),
                "case {sender_case:?}: expected a bus whose logging is off to write nothing, got \
                 {own:?}"
            );
            continue;
        }
        assert_one_record_per_event(
            &sender_case,
            &own,
            event_ids,
            sender.bus,
            bus_arn,
            events,
            sender.include_detail == INCLUDE_DETAIL_FULL,
        );
    }
}
