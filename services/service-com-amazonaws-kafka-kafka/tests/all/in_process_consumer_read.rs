// SPDX-License-Identifier: CC0-1.0

//! What an IN-PROCESS consumer of a cluster is handed, and the position it can move to:
//! the read a Lambda event source mapping of this emulator makes
//! ([`ComAmazonawsKafkaKafkaState::data_plane_fetch`]), rather than the `Fetch` a protocol
//! client sends.
//!
//! RULE under test: such a consumer is handed the records a client library would surface
//! to it — so a transaction's `COMMIT`/`ABORT` control batch, which is the protocol's own
//! marker and no record of the topic, and a record an abort discarded are not among them —
//! and the read also says how far it got PAST the records at the head of its window that
//! it surfaced none of. A consumer keeps its own position, so a window made only of such
//! records has to leave it somewhere past them: without that, a reader taking one record
//! at a time would read the same window on every pass and never reach the committed
//! record behind it.
//!
//! The rows span what the rule generalises over: a window holding a record an abort threw
//! away, one holding an `ABORT` marker, one holding a record a commit made visible, one
//! holding a `COMMIT` marker, and one past the end of the log.

use std::time::SystemTime;

use serde_json::json;
use service_com_amazonaws_kafka_kafka::state as kafka;
use service_com_amazonaws_kafka_kafka::state::broker_plane::{RecordProducer, SubmittedRecord};

use crate::kafka_harness::{pct, ClusterRequest, Emulator};

/// The topic the rows read, and the values the two transactions write.
const TOPIC: &str = "txn-consumed";
const ABORTED_VALUE: &str = "aborted-record";
const COMMITTED_VALUE: &str = "committed-record";

/// One window of the log: what it is called, the offset the consumer reads from, the
/// values it must be handed, and the position past what it was not handed.
type WindowRow<'a> = (&'a str, i64, &'a [&'a str], Option<i64>);

/// RULE: the committed record is the only one handed over, and every window before it
/// reports a position past itself.
#[tokio::test]
async fn an_in_process_consumer_is_handed_the_committed_records_and_can_move_past_the_rest() {
    let case = "a log an aborted and a committed transaction wrote";
    let emulator = Emulator::new();
    let cluster = emulator
        .create_ok(&ClusterRequest {
            client_broker: Some("PLAINTEXT"),
            ..ClusterRequest::named("txn-consumer-read")
        })
        .await;
    given_topic(&emulator, &cluster, TOPIC).await;

    // The log: an aborted transaction's record and its ABORT marker, then a committed
    // transaction's record and its COMMIT marker — offsets 0 to 3.
    let producer = RecordProducer {
        id: 4_200,
        epoch: 0,
        transactional: true,
        control: false,
    };
    transaction(&emulator, case, &cluster, producer, ABORTED_VALUE, false);
    transaction(&emulator, case, &cluster, producer, COMMITTED_VALUE, true);

    // (the offset the consumer reads from, the values it is handed, the position it may
    // move to past what it was not handed). One record per window, which is the smallest
    // batch a mapping may read with.
    let rows: &[WindowRow<'_>] = &[
        ("the aborted record", 0, &[], Some(0)),
        ("the abort marker", 1, &[], Some(1)),
        ("the committed record", 2, &[COMMITTED_VALUE], None),
        ("the commit marker", 3, &[], Some(3)),
        ("the end of the log", 4, &[], None),
    ];
    for (row, from, handed, through) in rows {
        let case = &format!("{case} / {row}");
        let read = {
            let clusters = kafka::service_state(&*emulator.engine)
                .unwrap_or_else(|| panic!("case {case:?}: the kafka service must be mounted"));
            let mut store = clusters.lock();
            store
                .data_plane_fetch(&cluster, TOPIC, 0, *from, 1, SystemTime::now())
                .unwrap_or_else(|refusal| {
                    panic!("case {case:?}: the read from {from} was refused: {refusal:?}")
                })
        };
        let values: Vec<String> = read
            .records
            .iter()
            .map(|record| {
                String::from_utf8_lossy(record.value.as_deref().unwrap_or_default()).to_string()
            })
            .collect();
        assert_eq!(
            values,
            handed.iter().map(ToString::to_string).collect::<Vec<String>>(),
            "case {case:?}: expected the consumer to be handed {handed:?}, got {values:?}"
        );
        assert_eq!(
            read.undelivered_through, *through,
            "case {case:?}: expected the position past what it was not handed to be \
             {through:?}, got {:?}",
            read.undelivered_through
        );
    }
}

/// Write one record inside a transaction of `producer` and end that transaction, through
/// the same append and the same marker a `Produce` and an `EndTxn` on the cluster's
/// bootstrap address write.
fn transaction(
    emulator: &Emulator,
    case: &str,
    cluster_arn: &str,
    producer: RecordProducer,
    value: &str,
    committed: bool,
) {
    let clusters = kafka::service_state(&*emulator.engine)
        .unwrap_or_else(|| panic!("case {case:?}: the kafka service must be mounted"));
    let now = SystemTime::now();
    let appended = clusters.lock().data_plane_produce(
        cluster_arn,
        TOPIC,
        0,
        vec![SubmittedRecord {
            timestamp_ms: -1,
            key: None,
            value: Some(value.as_bytes().to_vec()),
            headers: Vec::new(),
            codec: Default::default(),
            producer: Some(producer),
        }],
        now,
    );
    appended.unwrap_or_else(|refusal| {
        panic!("case {case:?}: producing {value:?} to {TOPIC} was refused: {refusal:?}")
    });
    let marker = clusters
        .lock()
        .data_plane_end_transaction(cluster_arn, TOPIC, 0, producer, committed, now);
    assert!(
        marker.is_some(),
        "case {case:?}: ending the transaction that wrote {value:?} must write its marker"
    );
}

/// A topic of the cluster, created through the control plane.
async fn given_topic(emulator: &Emulator, cluster: &str, name: &str) {
    let reply = emulator
        .call(
            "POST",
            &format!("/v1/clusters/{}/topics", pct(cluster)),
            Some(json!({
                "topicName": name,
                "partitionCount": 1,
                "replicationFactor": 1,
            })),
        )
        .await;
    assert_eq!(
        reply.status, 200,
        "setup: CreateTopic({name}) must be accepted, got {} {}",
        reply.status,
        reply.error_message()
    );
}
