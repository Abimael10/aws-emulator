// SPDX-License-Identifier: CC0-1.0

//! A bus whose `LogConfig` asks for logging, and a rule of that bus whose target is a
//! CloudWatch Logs LOG GROUP, write into CloudWatch Logs through TWO independent paths — end
//! to end over the wire (`awsJson1_1`, `POST /` with `X-Amz-Target: <prefix>.<Operation>`)
//! through the real `MultiServiceRouter` with CloudWatch Logs mounted on the same engine, so
//! both are asserted where a client reads them: the log events `FilterLogEvents` reports.
//!
//! The RULE asserted here: the records a bus's `LogConfig` vends and the log event a rule's
//! log-group target receives are decided, and written, independently of one another. The
//! bus's records travel to the log group of a delivery whose delivery source represents THAT
//! bus for a log type carrying the record's level, and the target's log event is written into
//! the group its own `Arn` names, carrying the text that target was handed. So neither group
//! holds the other's writes, each path writes whether or not the other one does, and a group
//! that is both a delivery's destination and a rule target's holds exactly the union of the
//! two.
//!
//! The tables span what the rule generalises over rather than the one recorded pipeline: the
//! rows differ along the bus's `Level` and `IncludeDetail`, along whether a vended delivery
//! exists at all and which log type its source carries, along whether the target group's
//! resource policy admits the write, along what the target asked to be handed (the whole
//! event envelope, an `InputTransformer` rendering a log record, one rendering no log record)
//! and along whether the published event matches the rule at all. Every row is a FRESH
//! emulator, so one row's pipeline never carries another row's records, and every row
//! publishes more than one event, so a count asserts one record PER event rather than one
//! record.
//!
//! The harness (the router with EventBridge and CloudWatch Logs on one engine, and the
//! EventBridge calls) is `tests/all/target_dispatch.rs`'s.

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::state::bus_logs::{
    ERROR_LOGS_LOG_TYPE, EVENT_INGEST_SUCCESS, INCLUDE_DETAIL_FULL, INFO_LOGS_LOG_TYPE,
    INVOCATION_FAILURE, LOG_LEVEL_ERROR, LOG_LEVEL_INFO,
};
use service_com_amazonaws_eventbridge_awsevents::state::{rule_arn, SERVICE_PRINCIPAL};

use crate::target_dispatch::{emulator, events, rpc, Emulator, DETAIL_TYPE, REGION, SOURCE};

/// The bus every row publishes onto, and the rule on it whose target is a log group.
const BUS: &str = "orders-bus";
const RULE: &str = "to-audit";

/// The id the rule's single target carries.
const TARGET_ID: &str = "audit-group";

/// The group a vended-log delivery carries the BUS's own records into.
const VENDED_GROUP: &str = "/aws/vendedlogs/events/orders-bus";

/// The group the RULE's target names, which is not where the bus's own records travel.
const TARGET_GROUP: &str = "/aws/events/audit";

/// The `IncludeDetail` value that carries no event detail (`INCLUDE_DETAIL_VALUES`).
const INCLUDE_DETAIL_NONE: &str = "NONE";

/// The two calls a delivery to a log-group target is made of, which are the actions the
/// group's resource policy has to admit for the write to happen.
const CREATE_LOG_STREAM: &str = "logs:CreateLogStream";
const PUT_LOG_EVENTS: &str = "logs:PutLogEvents";

/// A `DetailType` the rule's pattern does NOT match, so an event carrying it is a near miss:
/// the bus ingests it and no target of that rule is handed it.
const NEAR_MISS_DETAIL_TYPE: &str = "Order Cancelled";

/// One CloudWatch Logs call (`Logs_20140328`, awsJson1_1), asserting a 200 and answering the
/// body.
async fn logs(aws: &Emulator, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answer) = rpc(aws, "logs", "Logs_20140328", "1.1", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} answered {status} {answer}"
    );
    answer
}

/// One EventBridge call, asserting a 200 and answering the body.
async fn events_ok(aws: &Emulator, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answer) = events(aws, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} answered {status} {answer}"
    );
    answer
}

/// The bus [`BUS`], configured with `log_config` through `UpdateEventBus` so the
/// configuration a client turns on AFTER creating its bus is the one under test. Answers the
/// bus's ARN, and asserts the configuration reads back.
async fn given_bus(aws: &Emulator, case: &str, log_config: Option<Value>) -> String {
    events_ok(aws, case, "CreateEventBus", &json!({ "Name": BUS })).await;
    if let Some(config) = log_config.clone() {
        events_ok(
            aws,
            case,
            "UpdateEventBus",
            &json!({ "Name": BUS, "LogConfig": config }),
        )
        .await;
    }
    let described = events_ok(aws, case, "DescribeEventBus", &json!({ "Name": BUS })).await;
    let reported = described.get("LogConfig").cloned().unwrap_or(Value::Null);
    let expected = log_config.unwrap_or(Value::Null);
    assert_eq!(
        reported, expected,
        "case {case:?}: expected DescribeEventBus to report the configuration {expected}, got \
         {reported}"
    );
    described["Arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: DescribeEventBus reported no ARN: {described}"))
        .to_string()
}

/// A log group called `name`, answered with the ARN `DescribeLogGroups` reports for it — the
/// form carrying everything under the group, which is the ARN a client reads off that
/// listing.
async fn given_log_group(aws: &Emulator, case: &str, name: &str) -> String {
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
        .unwrap_or_else(|| panic!("case {case:?}: DescribeLogGroups reported no ARN: {listed}"))
        .to_string()
}

/// `group_arn` naming the group alone, which is the ARN a rule's target carries.
fn bare(group_arn: &str) -> String {
    group_arn.strip_suffix(":*").unwrap_or(group_arn).to_string()
}

/// `group_arn` naming the group together with everything under it, which is the resource a
/// grant for the write is written over.
fn everything_under(group_arn: &str) -> String {
    format!("{}:*", bare(group_arn))
}

/// Admit the two calls a log-group delivery is made of to the EventBridge service principal
/// on everything under `group_arn`, as a resource policy attached to that group.
async fn given_group_grant(aws: &Emulator, case: &str, group_arn: &str) {
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": SERVICE_PRINCIPAL },
            "Action": [CREATE_LOG_STREAM, PUT_LOG_EVENTS],
            "Resource": everything_under(group_arn),
        }],
    })
    .to_string();
    logs(
        aws,
        case,
        "PutResourcePolicy",
        &json!({
            "policyName": "events-delivery",
            "policyDocument": document,
            "resourceArn": group_arn,
        }),
    )
    .await;
}

/// The three calls a client builds a vended-log pipeline with: a delivery source over
/// `resource_arn` for `log_type`, a delivery destination over `group_arn`, and the delivery
/// pairing them.
async fn given_delivery(
    aws: &Emulator,
    case: &str,
    log_type: &str,
    resource_arn: &str,
    group_arn: &str,
) {
    logs(
        aws,
        case,
        "PutDeliverySource",
        &json!({ "name": "bus-logs", "resourceArn": resource_arn, "logType": log_type }),
    )
    .await;
    let destination = logs(
        aws,
        case,
        "PutDeliveryDestination",
        &json!({
            "name": "bus-logs-to-logs",
            "deliveryDestinationConfiguration": { "destinationResourceArn": group_arn },
        }),
    )
    .await;
    let destination_arn = destination["deliveryDestination"]["arn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: PutDeliveryDestination reported no ARN: {destination}")
        })
        .to_string();
    logs(
        aws,
        case,
        "CreateDelivery",
        &json!({
            "deliverySourceName": "bus-logs",
            "deliveryDestinationArn": destination_arn,
        }),
    )
    .await;
}

/// The rule [`RULE`] on [`BUS`], matching the published event's `Source` and `DetailType`,
/// carrying one target whose `Arn` is `target_arn` and which asks for `asked` beside it.
async fn given_rule_on_bus(aws: &Emulator, case: &str, target_arn: &str, asked: &Value) {
    let pattern = json!({ "source": [SOURCE], "detail-type": [DETAIL_TYPE] }).to_string();
    events_ok(
        aws,
        case,
        "PutRule",
        &json!({
            "Name": RULE,
            "EventBusName": BUS,
            "EventPattern": pattern,
            "State": "ENABLED",
        }),
    )
    .await;
    let mut target = json!({ "Id": TARGET_ID, "Arn": target_arn });
    for (member, value) in asked
        .as_object()
        .expect("the row's target members are an object")
    {
        target[member] = value.clone();
    }
    let answer = events_ok(
        aws,
        case,
        "PutTargets",
        &json!({ "Rule": RULE, "EventBusName": BUS, "Targets": [target] }),
    )
    .await;
    assert_eq!(
        answer["FailedEntryCount"],
        json!(0),
        "case {case:?}: PutTargets reported a failed entry: {answer}"
    );
}

/// The `Detail` of each event every row publishes: they differ in the value the target's
/// template selects, so a rendered message is attributed to the event it was rendered from.
const ORDER_IDS: [&str; 2] = ["A-1", "B-2"];

/// Publish one entry per [`ORDER_IDS`] member onto [`BUS`], each stamped at `at`, carrying
/// `detail_type`; answers the `EventId` of each in the request's order.
async fn publish(aws: &Emulator, case: &str, detail_type: &str, at: i64) -> Vec<String> {
    let entries: Vec<Value> = ORDER_IDS
        .iter()
        .map(|order| {
            json!({
                "EventBusName": BUS,
                "Source": SOURCE,
                "DetailType": detail_type,
                "Detail": json!({ "orderId": order }).to_string(),
                "Time": at,
            })
        })
        .collect();
    let answer = events_ok(aws, case, "PutEvents", &json!({ "Entries": entries })).await;
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

/// Every log event the group `group` holds, as `FilterLogEvents` reports them.
async fn written(aws: &Emulator, case: &str, group: &str) -> Vec<Value> {
    let answer = logs(
        aws,
        case,
        "FilterLogEvents",
        &json!({ "logGroupName": group }),
    )
    .await;
    answer["events"].as_array().cloned().unwrap_or_default()
}

/// The log events of `events` that are a BUS's own record: a JSON object naming what the bus
/// did and which resource it did it about, which is the envelope every vended record carries
/// and which no text handed to a target carries.
fn bus_records(events: &[Value]) -> Vec<Value> {
    events
        .iter()
        .filter_map(|event| {
            let message = event["message"].as_str()?;
            let parsed = serde_json::from_str::<Value>(message).ok()?;
            let object = parsed.as_object()?;
            if object.contains_key("message_type") && object.contains_key("resource_arn") {
                Some(parsed)
            } else {
                None
            }
        })
        .collect()
}

/// The bus records of `events` whose `message_type` is `message_type`.
fn of_type(events: &[Value], message_type: &str) -> Vec<Value> {
    bus_records(events)
        .into_iter()
        .filter(|record| record["message_type"] == json!(message_type))
        .collect()
}

/// A `LogConfig` as a client writes one: both members, which is the only form EventBridge
/// accepts.
fn log_config(level: &str, include_detail: &str) -> Value {
    json!({ "Level": level, "IncludeDetail": include_detail })
}

/// An instant inside the window CloudWatch Logs holds events for, read off the same clock the
/// emulator stamps its own records with, so the assertions never depend on a fixed date.
fn moments_ago() -> i64 {
    let now = ::std::time::SystemTime::now()
        .duration_since(::std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    i64::try_from(now).unwrap_or_default() - 30
}

/// What the rule's target asks to be handed, which is what decides the log record the target
/// group receives.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Asked {
    /// Nothing beside its `Arn`: the target is handed the whole event envelope.
    Envelope,
    /// An `InputTransformer` rendering the event's own instant and a message, which IS a log
    /// record.
    Record,
    /// An `InputTransformer` rendering neither of those members, so the rendering is no log
    /// record and there is nothing to write.
    NoRecord,
}

impl Asked {
    /// The target members this choice carries beside `Id` and `Arn`.
    fn members(self) -> Value {
        match self {
            Self::Envelope => json!({}),
            Self::Record => json!({
                "InputTransformer": {
                    "InputPathsMap": { "orderId": "$.detail.orderId" },
                    "InputTemplate": "{\"timestamp\":\"<aws.events.event.ingestion-time>\",\
                                       \"message\":\"order <orderId> placed\"}",
                },
            }),
            Self::NoRecord => json!({
                "InputTransformer": {
                    "InputPathsMap": { "orderId": "$.detail.orderId" },
                    "InputTemplate": "{\"order\":\"<orderId>\"}",
                },
            }),
        }
    }
}

/// One row of the independence table.
struct Row {
    case: &'static str,
    /// The bus's `LogConfig`, and `None` for a bus that configures no logging at all.
    config: Option<Value>,
    /// The `logType` the delivery source represents the bus under, and `None` for an account
    /// that built no vended-log pipeline at all.
    delivery: Option<&'static str>,
    /// Whether the TARGET group's resource policy admits the delivery's write.
    grants_target: bool,
    asked: Asked,
    /// Whether the published events match the rule's pattern.
    matches: bool,
    /// How many log events the TARGET group holds, over the two events published.
    target_events: usize,
    /// How many `EVENT_INGEST_SUCCESS` records the VENDED group holds.
    ingest_records: usize,
    /// How many `INVOCATION_FAILURE` records the VENDED group holds.
    failure_records: usize,
    /// Whether the ingest records carry the event's own `Detail`.
    carries_detail: bool,
}

/// RULE: the bus's vended records and the rule target's log event are written independently,
/// each into the group its OWN configuration names, so one group never holds the other's
/// writes and each path writes whether or not the other one does.
#[tokio::test]
async fn each_log_path_writes_only_into_the_group_its_own_configuration_names() {
    let rows = vec![
        Row {
            case: "informational logging with full detail, a delivery and a granted target",
            config: Some(log_config(LOG_LEVEL_INFO, INCLUDE_DETAIL_FULL)),
            delivery: Some(INFO_LOGS_LOG_TYPE),
            grants_target: true,
            asked: Asked::Envelope,
            matches: true,
            target_events: ORDER_IDS.len(),
            ingest_records: ORDER_IDS.len(),
            failure_records: 0,
            carries_detail: true,
        },
        Row {
            case: "informational logging without detail, and a target reshaping its input",
            config: Some(log_config(LOG_LEVEL_INFO, INCLUDE_DETAIL_NONE)),
            delivery: Some(INFO_LOGS_LOG_TYPE),
            grants_target: true,
            asked: Asked::Record,
            matches: true,
            target_events: ORDER_IDS.len(),
            ingest_records: ORDER_IDS.len(),
            failure_records: 0,
            carries_detail: false,
        },
        Row {
            case: "error logging alone, and a target group admitting nothing",
            config: Some(log_config(LOG_LEVEL_ERROR, INCLUDE_DETAIL_FULL)),
            delivery: Some(ERROR_LOGS_LOG_TYPE),
            grants_target: false,
            asked: Asked::Envelope,
            matches: true,
            target_events: 0,
            ingest_records: 0,
            failure_records: ORDER_IDS.len(),
            carries_detail: false,
        },
        Row {
            case: "informational logging, a granted target, and a near-miss event",
            config: Some(log_config(LOG_LEVEL_INFO, INCLUDE_DETAIL_FULL)),
            delivery: Some(INFO_LOGS_LOG_TYPE),
            grants_target: true,
            asked: Asked::Envelope,
            matches: false,
            target_events: 0,
            ingest_records: ORDER_IDS.len(),
            failure_records: 0,
            carries_detail: true,
        },
        Row {
            case: "no logging configured at all, and a granted target",
            config: None,
            delivery: Some(INFO_LOGS_LOG_TYPE),
            grants_target: true,
            asked: Asked::Envelope,
            matches: true,
            target_events: ORDER_IDS.len(),
            ingest_records: 0,
            failure_records: 0,
            carries_detail: false,
        },
        Row {
            case: "informational logging with no delivery at all, and a granted target",
            config: Some(log_config(LOG_LEVEL_INFO, INCLUDE_DETAIL_FULL)),
            delivery: None,
            grants_target: true,
            asked: Asked::Envelope,
            matches: true,
            target_events: ORDER_IDS.len(),
            ingest_records: 0,
            failure_records: 0,
            carries_detail: false,
        },
        Row {
            case: "informational logging, and a target whose rendering is no log record",
            config: Some(log_config(LOG_LEVEL_INFO, INCLUDE_DETAIL_FULL)),
            delivery: Some(INFO_LOGS_LOG_TYPE),
            grants_target: true,
            asked: Asked::NoRecord,
            matches: true,
            target_events: 0,
            ingest_records: ORDER_IDS.len(),
            failure_records: ORDER_IDS.len(),
            carries_detail: true,
        },
    ];

    for row in rows {
        let case = row.case;
        let aws = emulator();
        let bus_arn = given_bus(&aws, case, row.config.clone()).await;
        let target_arn = given_log_group(&aws, case, TARGET_GROUP).await;
        if row.grants_target {
            given_group_grant(&aws, case, &target_arn).await;
        }
        let target_arn = bare(&target_arn);
        given_rule_on_bus(&aws, case, &target_arn, &row.asked.members()).await;
        let vended_arn = given_log_group(&aws, case, VENDED_GROUP).await;
        if let Some(log_type) = row.delivery {
            given_delivery(&aws, case, log_type, &bus_arn, &vended_arn).await;
        }

        let at = moments_ago();
        let detail_type = if row.matches {
            DETAIL_TYPE
        } else {
            NEAR_MISS_DETAIL_TYPE
        };
        let event_ids = publish(&aws, case, detail_type, at).await;

        // The target's group holds the target's log events and nothing of the bus's own.
        let target_written = written(&aws, case, TARGET_GROUP).await;
        assert_eq!(
            target_written.len(),
            row.target_events,
            "case {case:?}: expected {} log event(s) in {TARGET_GROUP}, got {target_written:?}",
            row.target_events
        );
        let strayed = bus_records(&target_written);
        assert!(
            strayed.is_empty(),
            "case {case:?}: expected no bus record in {TARGET_GROUP}, got {strayed:?}"
        );
        for (index, event) in target_written.iter().enumerate() {
            let message = event["message"].as_str().unwrap_or_default();
            match row.asked {
                Asked::Record => {
                    let expected = format!("order {} placed", ORDER_IDS[index]);
                    assert_eq!(
                        message, expected,
                        "case {case:?}: expected the rendered message {expected:?}, got {event}"
                    );
                    assert_eq!(
                        event["timestamp"],
                        json!(at * 1_000),
                        "case {case:?}: expected the instant the template rendered ({}), got \
                         {event}",
                        at * 1_000
                    );
                }
                Asked::Envelope | Asked::NoRecord => {
                    let envelope = serde_json::from_str::<Value>(message).unwrap_or_else(|error| {
                        panic!("case {case:?}: the handed envelope is JSON ({error}): {message}")
                    });
                    assert_eq!(
                        envelope["id"],
                        json!(event_ids[index]),
                        "case {case:?}: expected the envelope of EventId {}, got {envelope}",
                        event_ids[index]
                    );
                    assert_eq!(
                        envelope["detail-type"],
                        json!(DETAIL_TYPE),
                        "case {case:?}: expected the published DetailType, got {envelope}"
                    );
                    assert_eq!(
                        envelope["detail"]["orderId"],
                        json!(ORDER_IDS[index]),
                        "case {case:?}: expected the published Detail, got {envelope}"
                    );
                }
            }
        }

        // The delivery's group holds the bus's own records and nothing of the target's.
        let vended_written = written(&aws, case, VENDED_GROUP).await;
        let ingested = of_type(&vended_written, EVENT_INGEST_SUCCESS);
        let failed = of_type(&vended_written, INVOCATION_FAILURE);
        assert_eq!(
            ingested.len(),
            row.ingest_records,
            "case {case:?}: expected {} {EVENT_INGEST_SUCCESS} record(s) in {VENDED_GROUP}, got \
             {vended_written:?}",
            row.ingest_records
        );
        assert_eq!(
            failed.len(),
            row.failure_records,
            "case {case:?}: expected {} {INVOCATION_FAILURE} record(s) in {VENDED_GROUP}, got \
             {vended_written:?}",
            row.failure_records
        );
        assert_eq!(
            vended_written.len(),
            ingested.len() + failed.len(),
            "case {case:?}: expected {VENDED_GROUP} to hold the bus's records alone, got \
             {vended_written:?}"
        );

        for record in ingested.iter().chain(failed.iter()) {
            assert_eq!(
                record["resource_arn"],
                json!(bus_arn),
                "case {case:?}: expected every record to name the bus {bus_arn}, got {record}"
            );
            assert_eq!(
                record["event_bus_name"],
                json!(BUS),
                "case {case:?}: expected every record to name the bus {BUS}, got {record}"
            );
            let about = record["event_id"].as_str().unwrap_or_default().to_string();
            assert!(
                event_ids.contains(&about),
                "case {case:?}: expected a record about one of {event_ids:?}, got {record}"
            );
        }
        for record in &ingested {
            assert_eq!(
                record["log_level"],
                json!(LOG_LEVEL_INFO),
                "case {case:?}: expected the informational level, got {record}"
            );
            let carried = record["details"].get("event_detail").cloned();
            assert_eq!(
                carried.is_some(),
                row.carries_detail,
                "case {case:?}: expected the event detail to be carried: {}, got {record}",
                row.carries_detail
            );
        }
        for record in &failed {
            assert_eq!(
                record["log_level"],
                json!(LOG_LEVEL_ERROR),
                "case {case:?}: expected the error level, got {record}"
            );
            assert_eq!(
                record["details"]["rule_arn"],
                json!(rule_arn(REGION, BUS, RULE)),
                "case {case:?}: expected the rule that matched, got {record}"
            );
            assert_eq!(
                record["details"]["target_arn"],
                json!(target_arn),
                "case {case:?}: expected the target the handover was made on, got {record}"
            );
            assert!(
                record["details"]["error_message"]
                    .as_str()
                    .is_some_and(|message| !message.is_empty()),
                "case {case:?}: expected the condition to report something, got {record}"
            );
        }
    }
}

/// RULE: the two paths are independent of the GROUP they write into as well, so one group
/// that is both a delivery's destination and a rule target's holds exactly the union — the
/// target's log events and the bus's own records, each written once.
#[tokio::test]
async fn one_group_serving_both_paths_holds_exactly_the_union_of_their_writes() {
    let case = "one group that is both the delivery's destination and the rule's target";
    let aws = emulator();
    let bus_arn = given_bus(
        &aws,
        case,
        Some(log_config(LOG_LEVEL_INFO, INCLUDE_DETAIL_FULL)),
    )
    .await;
    let group_arn = given_log_group(&aws, case, TARGET_GROUP).await;
    given_group_grant(&aws, case, &group_arn).await;
    let target_arn = bare(&group_arn);
    given_rule_on_bus(&aws, case, &target_arn, &Asked::Record.members()).await;
    given_delivery(&aws, case, INFO_LOGS_LOG_TYPE, &bus_arn, &group_arn).await;

    let at = moments_ago();
    let event_ids = publish(&aws, case, DETAIL_TYPE, at).await;
    let held = written(&aws, case, TARGET_GROUP).await;

    let ingested = of_type(&held, EVENT_INGEST_SUCCESS);
    assert_eq!(
        ingested.len(),
        event_ids.len(),
        "case {case:?}: expected one {EVENT_INGEST_SUCCESS} record per ingested event ({}), got \
         {held:?}",
        event_ids.len()
    );
    let rendered: Vec<&str> = held
        .iter()
        .filter_map(|event| event["message"].as_str())
        .filter(|message| serde_json::from_str::<Value>(message).is_err())
        .collect();
    let expected: Vec<String> = ORDER_IDS
        .iter()
        .map(|order| format!("order {order} placed"))
        .collect();
    assert_eq!(
        rendered, expected,
        "case {case:?}: expected the target's rendered records {expected:?}, got {held:?}"
    );
    assert_eq!(
        held.len(),
        ingested.len() + expected.len(),
        "case {case:?}: expected the group to hold exactly both paths' writes, got {held:?}"
    );
}
