// SPDX-License-Identifier: CC0-1.0

//! Unit cases over what a RabbitMQ event source mapping hands its function, and over the
//! two settings a pass reads off the mapping: the record form, the key the records sit
//! under, what a filter matches a message as, and the window a batch is gathered over.
//!
//! The data plane itself — a broker's own queue consumed, the function invoked and the
//! messages acknowledged — is asserted end to end over the AMQP wire in
//! `tests/all/mq_event_source_delivery.rs`.

use std::time::Duration;

use ::amq_protocol::protocol::basic::AMQPProperties;
use ::amq_protocol::types::{AMQPValue, FieldArray, FieldTable, LongString, ShortString};
use ::serde_json::{json, Value};
use ::service_com_amazonaws_mq_mq::state::message_broker::Message;

use super::{
    batching_window, filter_event, rabbitmq_event, rabbitmq_record, rendered_timestamp,
    BrokerTarget, DEFAULT_BATCHING_WINDOW, MAX_HEADER_DEPTH,
};

/// A message carrying `properties` and `body`, as a publish hands one over.
fn published(properties: AMQPProperties, body: &str) -> Message {
    Message::published("", "orders", properties, body.as_bytes().to_vec())
}

/// A target consuming `queue` in `virtual_host`, with everything else at what a mapping
/// configuring nothing has.
fn target(queue: &str, virtual_host: &str, filters: Vec<String>) -> BrokerTarget {
    BrokerTarget {
        uuid: "a8f7e2b1-0000-4000-8000-000000000001".to_string(),
        broker_arn: "arn:aws:mq:us-east-1:000000000000:broker:orders:b-1111".to_string(),
        broker_name: "orders".to_string(),
        region: "us-east-1".to_string(),
        queue: queue.to_string(),
        virtual_host: virtual_host.to_string(),
        function_arn: "arn:aws:lambda:us-east-1:000000000000:function:consumer".to_string(),
        batch_size: 10,
        batching_window: Duration::ZERO,
        filters,
    }
}

/// RULE: a record reports every AMQP basic property as the publish set it, `null` for one the
/// publish left out, whether the broker has already delivered the message once, the message's
/// own bytes base64-encoded and the size of those bytes.
///
/// The rows differ along what the rule generalises over: a publish that set no property at
/// all, one that set the text properties, one that set the numeric ones, and one that set a
/// header table holding a string, a number, an array and a nested table.
#[test]
fn a_record_reports_the_properties_the_publish_set() {
    // (case, the properties the publish carried, the members the record must report)
    let rows: Vec<(&str, AMQPProperties, Value)> = vec![
        (
            "no-property-at-all",
            AMQPProperties::default(),
            json!({
                "contentType": Value::Null,
                "contentEncoding": Value::Null,
                "headers": Value::Null,
                "deliveryMode": Value::Null,
                "priority": Value::Null,
                "correlationId": Value::Null,
                "replyTo": Value::Null,
                "expiration": Value::Null,
                "messageId": Value::Null,
                "timestamp": Value::Null,
                "type": Value::Null,
                "userId": Value::Null,
                "appId": Value::Null,
                "clusterId": Value::Null,
                "bodySize": 5,
            }),
        ),
        (
            "the-text-properties",
            AMQPProperties::default()
                .with_content_type(ShortString::from("application/json"))
                .with_content_encoding(ShortString::from("identity"))
                .with_correlation_id(ShortString::from("corr-1"))
                .with_reply_to(ShortString::from("replies"))
                .with_expiration(ShortString::from("60000"))
                .with_message_id(ShortString::from("msg-1"))
                .with_type(ShortString::from("order"))
                .with_user_id(ShortString::from("broker-user"))
                .with_app_id(ShortString::from("producer"))
                .with_cluster_id(ShortString::from("cluster-1")),
            json!({
                "contentType": "application/json",
                "contentEncoding": "identity",
                "headers": Value::Null,
                "deliveryMode": Value::Null,
                "priority": Value::Null,
                "correlationId": "corr-1",
                "replyTo": "replies",
                "expiration": "60000",
                "messageId": "msg-1",
                "timestamp": Value::Null,
                "type": "order",
                "userId": "broker-user",
                "appId": "producer",
                "clusterId": "cluster-1",
                "bodySize": 5,
            }),
        ),
        (
            "the-numeric-properties-and-the-timestamp",
            AMQPProperties::default()
                .with_delivery_mode(2)
                .with_priority(34)
                .with_timestamp(2021),
            json!({
                "contentType": Value::Null,
                "contentEncoding": Value::Null,
                "headers": Value::Null,
                "deliveryMode": 2,
                "priority": 34,
                "correlationId": Value::Null,
                "replyTo": Value::Null,
                "expiration": Value::Null,
                "messageId": Value::Null,
                "timestamp": "Jan 1, 1970, 12:33:41 AM",
                "type": Value::Null,
                "userId": Value::Null,
                "appId": Value::Null,
                "clusterId": Value::Null,
                "bodySize": 5,
            }),
        ),
        (
            "a-header-table-of-every-shape",
            AMQPProperties::default().with_headers(headers()),
            json!({
                "contentType": Value::Null,
                "contentEncoding": Value::Null,
                "headers": {
                    "header1": { "bytes": [118, 97, 108, 117, 101, 49] },
                    "nested": { "inner": { "bytes": [105, 110] } },
                    "numbers": [3, 4],
                    "retries": 7,
                },
                "deliveryMode": Value::Null,
                "priority": Value::Null,
                "correlationId": Value::Null,
                "replyTo": Value::Null,
                "expiration": Value::Null,
                "messageId": Value::Null,
                "timestamp": Value::Null,
                "type": Value::Null,
                "userId": Value::Null,
                "appId": Value::Null,
                "clusterId": Value::Null,
                "bodySize": 5,
            }),
        ),
    ];
    for (case, properties, expected) in rows {
        let record = rabbitmq_record(&published(properties, "hello"));
        assert_eq!(
            record["basicProperties"], expected,
            "case {case:?}: expected the properties {expected}, got {}",
            record["basicProperties"]
        );
        assert_eq!(
            record["redelivered"],
            json!(false),
            "case {case:?}: a message nothing has delivered yet is not redelivered, got {}",
            record["redelivered"]
        );
        assert_eq!(
            record["data"],
            json!("aGVsbG8="),
            "case {case:?}: expected the message's own bytes base64-encoded, got {}",
            record["data"]
        );
    }
}

/// A header table holding a string, a number, an array of numbers and a nested table.
fn headers() -> FieldTable {
    let mut nested = FieldTable::default();
    nested.insert(
        ShortString::from("inner"),
        AMQPValue::LongString(LongString::from("in")),
    );
    let mut table = FieldTable::default();
    table.insert(
        ShortString::from("header1"),
        AMQPValue::LongString(LongString::from("value1")),
    );
    table.insert(ShortString::from("retries"), AMQPValue::LongInt(7));
    table.insert(
        ShortString::from("numbers"),
        AMQPValue::FieldArray(FieldArray::from(vec![
            AMQPValue::LongInt(3),
            AMQPValue::LongInt(4),
        ])),
    );
    table.insert(ShortString::from("nested"), AMQPValue::FieldTable(nested));
    table
}

/// RULE: the event a batch is handed over as names the source and holds its records under the
/// QUEUE they came from and the virtual host that queue lives in, in the order the queue
/// handed them over.
///
/// The rows differ along what the rule generalises over: how many messages the batch holds,
/// and whether the mapping consumes the broker's default virtual host or one of its own.
#[test]
fn the_event_keys_its_records_by_the_queue_and_its_virtual_host() {
    // (case, the queue, the virtual host, the bodies of the batch, the key the records sit
    // under)
    let rows: Vec<(&str, &str, &str, Vec<&str>, &str)> = vec![
        ("one-message", "orders", "/", vec!["one"], "orders::/"),
        (
            "three-messages-in-order",
            "orders",
            "/",
            vec!["one", "two", "three"],
            "orders::/",
        ),
        (
            "a-virtual-host-of-its-own",
            "pizzaQueue",
            "shop",
            vec!["one"],
            "pizzaQueue::shop",
        ),
    ];
    for (case, queue, virtual_host, bodies, key) in rows {
        let batch: Vec<Message> = bodies
            .iter()
            .map(|body| published(AMQPProperties::default(), body))
            .collect();
        let event = rabbitmq_event(&target(queue, virtual_host, Vec::new()), &batch);
        assert_eq!(
            event["eventSource"],
            json!("aws:rmq"),
            "case {case:?}: expected the RabbitMQ event source, got {}",
            event["eventSource"]
        );
        assert_eq!(
            event["eventSourceArn"],
            json!("arn:aws:mq:us-east-1:000000000000:broker:orders:b-1111"),
            "case {case:?}: expected the broker's own ARN, got {}",
            event["eventSourceArn"]
        );
        let under: Vec<String> = event["rmqMessagesByQueue"]
            .as_object()
            .map(|by_queue| by_queue.keys().cloned().collect())
            .unwrap_or_default();
        assert_eq!(
            under,
            vec![key.to_string()],
            "case {case:?}: expected the records under {key:?}, got {under:?}"
        );
        let records = event["rmqMessagesByQueue"][key]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let carried: Vec<String> = records
            .iter()
            .map(|record| record["data"].as_str().unwrap_or_default().to_string())
            .collect();
        let expected: Vec<String> = bodies
            .iter()
            .map(|body| {
                ::base64::Engine::encode(
                    &::base64::engine::general_purpose::STANDARD,
                    body.as_bytes(),
                )
            })
            .collect();
        assert_eq!(
            carried, expected,
            "case {case:?}: expected the bodies {expected:?} in order, got {carried:?}"
        );
    }
}

/// RULE: a header table is rendered to the depth a record reports and no deeper — how deeply a
/// header NESTS is the publishing client's choice, so a table within the bound is rendered and
/// one past it is reported as no value at all rather than walked.
///
/// The rows differ along what the rule generalises over: one level, each side of the published
/// bound, and a depth far past it.
#[test]
fn a_header_nested_past_the_bound_is_reported_as_no_value() {
    // (case, how deeply the header nests, the value its innermost entry is rendered as)
    let rows: Vec<(&str, usize, Value)> = vec![
        ("one-level", 1, json!(7)),
        ("at-the-bound", MAX_HEADER_DEPTH, json!(7)),
        ("one-past-the-bound", MAX_HEADER_DEPTH + 1, Value::Null),
        ("far-past-the-bound", MAX_HEADER_DEPTH * 8, Value::Null),
    ];
    for (case, depth, expected) in rows {
        let record = rabbitmq_record(&published(
            AMQPProperties::default().with_headers(nested_headers(depth)),
            "hello",
        ));
        let mut rendered = &record["basicProperties"]["headers"];
        for _ in 1..depth {
            rendered = &rendered["nested"];
        }
        let leaf = &rendered["leaf"];
        assert_eq!(
            leaf, &expected,
            "case {case:?}: expected the innermost header to be rendered as {expected}, got \
             {leaf}"
        );
    }
}

/// A header table nested `depth` levels deep, each level under `nested`, whose innermost entry
/// is a number under `leaf`.
fn nested_headers(depth: usize) -> FieldTable {
    let mut table = FieldTable::default();
    table.insert(ShortString::from("leaf"), AMQPValue::LongInt(7));
    for _ in 1..depth {
        let mut outer = FieldTable::default();
        outer.insert(ShortString::from("nested"), AMQPValue::FieldTable(table));
        table = outer;
    }
    table
}

/// RULE: a message is matched against the record the function receives, with a `data` that
/// holds a JSON object or array read as that document — so a filter names the fields inside a
/// structured message — and left as the base64 text it is for a body that is anything else.
///
/// The rows differ along what the rule generalises over: a body that is a JSON object, one
/// that is an array, one that is a bare scalar, one that is not JSON at all, and a clause on
/// a property rather than on the body.
#[test]
fn a_structured_body_is_matched_as_the_document_it_spells() {
    // (case, the message body, the pattern, whether the message is admitted)
    let rows: Vec<(&str, &str, &str, bool)> = vec![
        (
            "an-object-body-a-clause-matches",
            r#"{"city":"Seattle"}"#,
            r#"{"data":{"city":["Seattle"]}}"#,
            true,
        ),
        (
            "an-object-body-no-clause-matches",
            r#"{"city":"Dublin"}"#,
            r#"{"data":{"city":["Seattle"]}}"#,
            false,
        ),
        (
            "an-array-body",
            r#"[{"city":"Seattle"}]"#,
            r#"{"data":{"city":["Seattle"]}}"#,
            true,
        ),
        (
            "a-bare-scalar-body-is-not-read-as-a-document",
            "41",
            r#"{"data":{"city":["Seattle"]}}"#,
            false,
        ),
        (
            "a-body-that-is-not-json-at-all",
            "not json",
            r#"{"data":{"city":["Seattle"]}}"#,
            false,
        ),
        (
            "a-clause-on-a-property-rather-than-the-body",
            "not json",
            r#"{"redelivered":[false]}"#,
            true,
        ),
    ];
    for (case, body, pattern, expected) in rows {
        let criteria = crate::state::event_source_filter::Filters::of(
            case,
            &[pattern.to_string()],
        );
        let mut budget = crate::state::event_source_filter::budget();
        let message = published(AMQPProperties::default(), body);
        let admitted = criteria.admits(&filter_event(&message), &mut budget);
        assert_eq!(
            admitted, expected,
            "case {case:?}: expected the message to be admitted: {expected}, got {admitted}"
        );
    }
}

/// RULE: a mapping on a broker that states no `MaximumBatchingWindowInSeconds` gathers over
/// this source's own default window of 500 ms, and one that states a span gathers over that
/// span, read inside the range the member admits.
///
/// The rows differ along what the rule generalises over: no window at all, each published
/// bound of the member's `@range(min: 0, max: 300)`, a span between them, and values outside
/// the range at either end.
#[test]
fn the_window_a_batch_is_gathered_over_is_the_one_the_mapping_states() {
    // (case, the stored `MaximumBatchingWindowInSeconds`, the window a pass gathers over)
    let rows: Vec<(&str, Option<i32>, Duration)> = vec![
        ("no-window-stated", None, DEFAULT_BATCHING_WINDOW),
        ("the-published-minimum", Some(0), Duration::ZERO),
        ("a-span-between-the-bounds", Some(12), Duration::from_secs(12)),
        ("the-published-maximum", Some(300), Duration::from_secs(300)),
        ("one-past-the-maximum", Some(301), Duration::from_secs(300)),
        ("one-below-the-minimum", Some(-1), Duration::ZERO),
    ];
    for (case, configured, expected) in rows {
        let window = batching_window(configured);
        assert_eq!(
            window, expected,
            "case {case:?}: expected a window of {expected:?}, got {window:?}"
        );
    }
}

/// RULE: an AMQP `timestamp` property is rendered as the month, the day and the year, then
/// the time of day on a twelve-hour clock.
///
/// The rows differ along what the rule generalises over: midnight, the documented value, noon,
/// an hour past noon and an instant in another month and year.
#[test]
fn a_timestamp_property_is_rendered_on_a_twelve_hour_clock() {
    // (case, the epoch seconds the property carries, the rendering)
    let rows: Vec<(&str, u64, &str)> = vec![
        ("midnight", 0, "Jan 1, 1970, 12:00:00 AM"),
        ("the-documented-value", 2021, "Jan 1, 1970, 12:33:41 AM"),
        ("noon", 43_200, "Jan 1, 1970, 12:00:00 PM"),
        ("an-hour-past-noon", 46_800, "Jan 1, 1970, 1:00:00 PM"),
        (
            "another-month-and-year",
            1_700_000_000,
            "Nov 14, 2023, 10:13:20 PM",
        ),
    ];
    for (case, seconds, expected) in rows {
        let rendered = rendered_timestamp(seconds);
        assert_eq!(
            rendered.as_deref(),
            Some(expected),
            "case {case:?}: expected {expected:?}, got {rendered:?}"
        );
    }
}
