// SPDX-License-Identifier: CC0-1.0

//! What this reader builds out of one record of an Amazon MSK topic, and which mappings it
//! reads at all.
//!
//! The delivery itself — a mapping that consumes a cluster's topic and invokes its function,
//! what its `FilterCriteria` admit of a batch and the position it resumes from after a pause —
//! is asserted end to end over the wire in
//! `tests/all/msk_event_source_mapping_delivery.rs`, with the records produced
//! through the cluster's own published Apache Kafka endpoint. What is here is the reading of
//! ONE record and of ONE mapping, which that suite cannot vary as cheaply.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use ::serde_json::{json, Value};

use super::{
    batching_window, filter_event, kafka_event, named_cluster, partition_key, wants_polling,
    ClusterPlane, ClusterTarget, ConsumedAccess, StoredRecord,
};
use crate::state::ShardCursor;

/// The cluster every row below reads, and the Region it lives in.
const CLUSTER: &str = "arn:aws:kafka:us-east-1:000000000000:cluster/orders/7b1f";
/// See [`CLUSTER`].
const REGION: &str = "us-east-1";

/// The topic every row below reads, and the function it is handed to.
const TOPIC: &str = "orders";
/// See [`TOPIC`].
const FUNCTION: &str = "arn:aws:lambda:us-east-1:000000000000:function:consume";

/// A mapping configuration the rows below are built against.
fn target() -> ClusterTarget {
    ClusterTarget {
        uuid: "0e6b8f41-9bb0-4a38-9a9c-2f6b8c1d4e55".to_string(),
        cluster_arn: CLUSTER.to_string(),
        region: REGION.to_string(),
        topics: vec![TOPIC.to_string()],
        function_arn: FUNCTION.to_string(),
        batch_size: 10,
        batching_window: Duration::ZERO,
        filters: Vec::new(),
        reports_batch_item_failures: false,
        meters_event_counts: false,
        access: ConsumedAccess::Iam,
        consumer_group: "0e6b8f41-9bb0-4a38-9a9c-2f6b8c1d4e55".to_string(),
        consumer_client: "lambda-event-source-mapping-0e6b8f41".to_string(),
        cursors: BTreeMap::new(),
        initial_cursor: ShardCursor::TrimHorizon,
    }
}

/// The published brokers a row's event reports, as the cluster's own endpoint publishes them.
fn plane() -> ClusterPlane {
    ClusterPlane {
        bootstrap_servers: Some("127.0.0.1:50001,127.0.0.1:50002".to_string()),
        partitions: vec![(TOPIC.to_string(), 0)],
    }
}

/// One record of the log, with everything a producer may leave out left out.
fn record(offset: i64, value: Option<&str>) -> StoredRecord {
    StoredRecord {
        offset,
        timestamp_ms: 1_700_000_000_000,
        log_append_time: false,
        key: None,
        value: value.map(|text| text.as_bytes().to_vec()),
        headers: Vec::new(),
        codec: Default::default(),
    }
}

/// RULE: a batch is handed over as the event Lambda gives a Kafka source — the source it came
/// from, the addresses its consumer reached the brokers at, and its records keyed by the topic
/// and partition they came from, each carrying where it sits in the log, when and by whose
/// clock it was stamped, its own key and value base64-encoded — with no value for a member the
/// record carries none for — and its headers in the producer's order.
///
/// The rows differ along what the rule generalises over: whether the record carries a key,
/// whether it carries a value at all, whose clock stamped it, and how many headers it holds.
#[test]
fn a_batch_is_handed_over_as_the_event_a_kafka_source_gives() {
    let keyed = StoredRecord {
        key: Some(b"order-1".to_vec()),
        ..record(4, Some(r#"{"total":10}"#))
    };
    let broker_stamped = StoredRecord {
        log_append_time: true,
        ..record(5, Some("plain"))
    };
    let with_headers = StoredRecord {
        headers: vec![
            ("trace".to_string(), Some(b"abc".to_vec())),
            ("flag".to_string(), None),
        ],
        ..record(6, Some("plain"))
    };
    let tombstone = record(7, None);
    // (case, the record, what its envelope must carry)
    let rows: Vec<(&str, StoredRecord, Value)> = vec![
        (
            "a keyed record carrying a document",
            keyed,
            json!({
                "topic": TOPIC,
                "partition": 0,
                "offset": 4,
                "timestamp": 1_700_000_000_000_i64,
                "timestampType": "CREATE_TIME",
                "headers": [],
                "key": "b3JkZXItMQ==",
                "value": "eyJ0b3RhbCI6MTB9",
            }),
        ),
        (
            "a record the broker stamped itself",
            broker_stamped,
            json!({
                "topic": TOPIC,
                "partition": 0,
                "offset": 5,
                "timestamp": 1_700_000_000_000_i64,
                "timestampType": "LOG_APPEND_TIME",
                "headers": [],
                "key": null,
                "value": "cGxhaW4=",
            }),
        ),
        (
            "a record carrying headers, one of them without a value",
            with_headers,
            json!({
                "topic": TOPIC,
                "partition": 0,
                "offset": 6,
                "timestamp": 1_700_000_000_000_i64,
                "timestampType": "CREATE_TIME",
                "headers": [{ "trace": [97, 98, 99] }, { "flag": null }],
                "key": null,
                "value": "cGxhaW4=",
            }),
        ),
        (
            "a tombstone, which carries no value at all",
            tombstone,
            json!({
                "topic": TOPIC,
                "partition": 0,
                "offset": 7,
                "timestamp": 1_700_000_000_000_i64,
                "timestampType": "CREATE_TIME",
                "headers": [],
                "key": null,
                "value": null,
            }),
        ),
    ];
    for (case, held, expected) in rows {
        let event = kafka_event(&target(), &plane(), TOPIC, 0, std::slice::from_ref(&held));
        assert_eq!(
            event["eventSource"],
            json!("aws:kafka"),
            "case {case:?}: expected the Kafka event source, got {event}"
        );
        assert_eq!(
            event["eventSourceArn"],
            json!(CLUSTER),
            "case {case:?}: expected the cluster the mapping names, got {event}"
        );
        assert_eq!(
            event["bootstrapServers"],
            json!("127.0.0.1:50001,127.0.0.1:50002"),
            "case {case:?}: expected the published brokers, got {event}"
        );
        let envelopes = &event["records"][partition_key(TOPIC, 0)];
        assert_eq!(
            envelopes,
            &json!([expected]),
            "case {case:?}: expected the record keyed by its topic and partition as \
             {expected}, got {event}"
        );
    }
}

/// RULE: the criteria decide the record's own members, with the bytes its key and value carry
/// read as the document those bytes spell where they spell an object or a list, and left as
/// the text they travel as where they do not.
///
/// The rows differ along what the rule generalises over: a value that is an object, one that
/// is a list, one that is a bare scalar, one that is no text at all, a KEY that is a document,
/// and a record with no value to read.
#[test]
fn the_filter_reads_a_payload_as_the_document_its_bytes_spell() {
    let bytes = |value: Vec<u8>| StoredRecord {
        value: Some(value),
        ..record(1, None)
    };
    // (case, the record, what the filter must see under `value`)
    let rows: Vec<(&str, StoredRecord, Value)> = vec![
        (
            "a value spelling an object",
            bytes(br#"{"department":"finance"}"#.to_vec()),
            json!({ "department": "finance" }),
        ),
        (
            "a value spelling a list",
            bytes(b"[1,2]".to_vec()),
            json!([1, 2]),
        ),
        (
            "a value spelling a bare scalar",
            bytes(b"12".to_vec()),
            json!("MTI="),
        ),
        (
            "a value that is no text at all",
            bytes(vec![0xff, 0xfe]),
            json!("//4="),
        ),
        ("a record with no value", record(1, None), Value::Null),
    ];
    for (case, held, expected) in rows {
        let event = filter_event(TOPIC, 0, &held);
        let seen = event.get("value").cloned().unwrap_or(Value::Null);
        assert_eq!(
            seen, expected,
            "case {case:?}: expected the filter to see {expected}, got {seen}"
        );
    }
    // The KEY is read by the same rule, so a clause may name fields inside a structured key.
    let keyed = StoredRecord {
        key: Some(br#"{"tenant":"acme"}"#.to_vec()),
        ..record(2, Some("plain"))
    };
    let event = filter_event(TOPIC, 0, &keyed);
    assert_eq!(
        event["key"],
        json!({ "tenant": "acme" }),
        "case \"a key spelling an object\": expected the key read as that document, got {event}"
    );
}

/// RULE: a mapping on a Kafka source gathers one batch over the window it states, and over
/// this source's own documented default of 500 ms when it states none; the value is read
/// inside the range the member admits.
#[test]
fn the_batching_window_is_the_sources_own_default_or_the_one_the_mapping_states() {
    // (case, `MaximumBatchingWindowInSeconds` as stored, the window one batch is gathered over)
    let rows: Vec<(&str, Option<i32>, Duration)> = vec![
        ("no window stated", None, Duration::from_millis(500)),
        ("a window of zero", Some(0), Duration::ZERO),
        ("a window of one second", Some(1), Duration::from_secs(1)),
        (
            "the largest window the member admits",
            Some(300),
            Duration::from_secs(300),
        ),
        (
            "a window past what the member admits",
            Some(301),
            Duration::from_secs(300),
        ),
        ("a window below zero", Some(-5), Duration::ZERO),
    ];
    for (case, stated, expected) in rows {
        let window = batching_window(stated);
        assert_eq!(
            window, expected,
            "case {case:?}: expected a window of {expected:?}, got {window:?}"
        );
    }
}

/// RULE: this reader polls a mapping that is `Enabled`, is not being deleted, and names an
/// Amazon MSK CLUSTER of this account — and no other mapping.
///
/// The rows differ along what the rule generalises over: the source the mapping names (a
/// cluster, the cross-account VPC connection form, another vendor's resource, no ARN at all)
/// and whether polling is paused or the mapping is being torn down.
#[test]
fn only_an_enabled_mapping_on_a_cluster_of_this_account_is_polled_here() {
    // (case, `EventSourceArn` as stored, `Enabled`, whether a delete was asked for, polled)
    let rows: Vec<(&str, Option<&str>, bool, bool, bool)> = vec![
        ("an enabled mapping on a cluster", Some(CLUSTER), true, false, true),
        ("the same mapping with polling paused", Some(CLUSTER), false, false, false),
        ("the same mapping being deleted", Some(CLUSTER), true, true, false),
        (
            "a cross-account VPC connection, whose cluster is another account's",
            Some("arn:aws:kafka:us-east-1:000000000000:vpc-connection/orders/7b1f"),
            true,
            false,
            false,
        ),
        (
            "another vendor's resource",
            Some("arn:aws:sqs:us-east-1:000000000000:orders"),
            true,
            false,
            false,
        ),
        ("a self-managed cluster, which carries no ARN", None, true, false, false),
    ];
    for (case, source, enabled, deleting, expected) in rows {
        let mut mapping = crate::state::EventSourceMappingRecord {
            event_source_arn: source.map(str::to_string),
            enabled,
            ..mapping_record()
        };
        if deleting {
            mapping.begin_delete(SystemTime::now());
        }
        let polled = wants_polling(&mapping);
        assert_eq!(
            polled, expected,
            "case {case:?}: expected this reader to poll the mapping: {expected}, got {polled}"
        );
    }
}

/// RULE: the Region a cluster is reached in is the one its own ARN names, and a value naming
/// no cluster of this emulator names no Region at all.
#[test]
fn a_cluster_is_reached_in_the_region_its_own_arn_names() {
    // (case, the value, the Region it is reached in)
    let rows: Vec<(&str, &str, Option<&str>)> = vec![
        ("a cluster of this Region", CLUSTER, Some(REGION)),
        (
            "a cluster of another Region",
            "arn:aws:kafka:eu-west-1:000000000000:cluster/orders/7b1f",
            Some("eu-west-1"),
        ),
        (
            "a VPC connection",
            "arn:aws:kafka:us-east-1:000000000000:vpc-connection/orders/7b1f",
            None,
        ),
        ("a value that is no ARN", "orders", None),
    ];
    for (case, value, expected) in rows {
        let region = named_cluster(value);
        assert_eq!(
            region, expected,
            "case {case:?}: expected the Region {expected:?}, got {region:?}"
        );
    }
}

/// A stored mapping the rows above vary one member of.
fn mapping_record() -> crate::state::EventSourceMappingRecord {
    let now = SystemTime::now();
    crate::state::EventSourceMappingRecord {
        uuid: "0e6b8f41-9bb0-4a38-9a9c-2f6b8c1d4e55".to_string(),
        arn: "arn:aws:lambda:us-east-1:000000000000:event-source-mapping:0e6b8f41".to_string(),
        function_arn: FUNCTION.to_string(),
        function_name: "consume".to_string(),
        function_qualifier: None,
        event_source_arn: Some(CLUSTER.to_string()),
        enabled: true,
        batch_size: 100,
        maximum_batching_window_in_seconds: None,
        parallelization_factor: None,
        starting_position: Some("TRIM_HORIZON".to_string()),
        starting_position_timestamp: None,
        filter_criteria: None,
        kms_key_arn: None,
        metrics_config: None,
        logging_config: None,
        scaling_config: None,
        destination_config: None,
        maximum_record_age_in_seconds: None,
        bisect_batch_on_function_error: None,
        maximum_retry_attempts: None,
        tumbling_window_in_seconds: None,
        topics: Some(vec![TOPIC.to_string()]),
        queues: None,
        source_access_configurations: None,
        self_managed_event_source: None,
        function_response_types: Vec::new(),
        amazon_managed_kafka_event_source_config: None,
        self_managed_kafka_event_source_config: None,
        document_db_event_source_config: None,
        provisioned_poller_config: None,
        tags: BTreeMap::new(),
        state: "Enabled".to_string(),
        state_transition_reason: "USER_INITIATED".to_string(),
        settled_due: now,
        removal_due: None,
        last_modified: now,
        created_at: now,
        shard_cursors: BTreeMap::new(),
        last_processing_result: None,
    }
}
