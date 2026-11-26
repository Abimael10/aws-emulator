// SPDX-License-Identifier: CC0-1.0

//! What an event source mapping on an Amazon MSK topic is handed when a TRANSACTIONAL
//! producer writes that topic, on the same services the delivery suite mounts
//! ([`crate::msk_event_source_mapping_delivery::emulator_on`]).
//!
//! RULE under test: a consumer of a Kafka partition is handed the records a committed
//! reader is given — a transaction's `COMMIT`/`ABORT` control batch is the protocol's own
//! marker and no record of the topic, and a record an abort discarded was thrown away —
//! and its POSITION moves past the ones it is not handed. A client library advances its
//! own position past a control batch and an aborted record, so a consumer whose batch
//! window holds only such records must move past that window rather than reading it again
//! on every pass: a mapping reading a topic one record at a time has to get past an
//! aborted transaction and its marker to reach the committed record behind them.
//!
//! The rows of the log span what the rule generalises over: a record an abort threw away,
//! the two kinds of control batch a transaction ends with, and a record a commit made
//! visible — with `BatchSize` 1, so every one of them is a window of its own and the
//! position has to move through each.
//!
//! The verdict is computed from what the HANDLER was handed: it refuses an event carrying
//! anything but the committed record, so the mapping reports its invocation `OK` only
//! where exactly that record was delivered, and from the GROUP's committed offset, which
//! is where every reader of the cluster sees how far the mapping has got.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use kafka_protocol::messages::{offset_fetch_request::OffsetFetchRequestTopic, OffsetFetchRequest};
use kafka_protocol::messages::{GroupId, TopicName};
use kafka_protocol::protocol::StrBytes;
use serde_json::json;
use service_com_amazonaws_kafka_kafka::state as kafka;
use service_com_amazonaws_kafka_kafka::state::broker_plane::{RecordProducer, SubmittedRecord};

use crate::event_source_mappings::{
    await_mapping_state, create_mapping, create_running_function, get_mapping, LOOK_AGAIN,
    STATE_ENABLED,
};
use crate::msk_event_source_mapping_delivery::{emulator_on, given_cluster, given_topic};

/// How long a wait for work the mapping has to do is given before the suite is a failure.
/// Well above every settle span the chain crosses — the cluster's, the topic's, the
/// function's, the mapping's and the poller's interval — so it fails only when the work
/// was not done at all.
const BUDGET: Duration = Duration::from_secs(60);

/// How long the function below is given to run one batch.
const FUNCTION_TIMEOUT: i64 = 60;

/// The topic the mapping consumes, and the group it consumes it as.
const TOPIC: &str = "txn-orders";
/// See [`TOPIC`].
const GROUP: &str = "msk-txn-source-consumers";

/// The values the transactions below write: one a transaction ABORTED, one a transaction
/// COMMITTED.
const ABORTED_VALUE: &str = "aborted-record";
const COMMITTED_VALUE: &str = "committed-record";

/// The handler the mapping's function runs: it reports the values it was handed on a line
/// of its own and refuses an event carrying anything but the committed record, so an
/// invocation succeeds only where exactly that record was handed over — a control batch or
/// an aborted record reaching it fails the invocation, which stops the mapping's position
/// instead of moving it.
const CHECKING_HANDLER: &str = r#"
import base64

def lambda_handler(event, context):
    handed = [
        base64.b64decode(record["value"]).decode()
        for batch in event["records"].values()
        for record in batch
    ]
    print("handed:" + ",".join(handed))
    if handed != ["committed-record"]:
        raise Exception("the mapping handed over %r" % (handed,))
    return {"handled": handed}
"#;

/// The line that handler writes, which is how this suite reads what the mapping handed it.
const HANDED_MARKER: &str = "handed:";

/// RULE: the mapping is handed the committed record alone, and its position moves past the
/// aborted record and both control batches rather than stalling on them.
#[tokio::test]
async fn a_mapping_reads_past_the_records_a_transaction_left_undelivered() {
    let case = "a topic written by a transactional producer";
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(&engine);
    let cluster = given_cluster(&router, case, "msk-txn-reader").await;
    given_topic(&router, case, &cluster, TOPIC, 1).await;
    let function = "msk-txn-reader-fn";
    create_running_function(
        &router,
        case,
        function,
        CHECKING_HANDLER,
        FUNCTION_TIMEOUT,
        &json!({}),
    )
    .await;

    // The log the mapping reads: an aborted transaction's record and its ABORT marker,
    // then a committed transaction's record and its COMMIT marker — offsets 0 to 3.
    let producer = RecordProducer {
        id: 7_000,
        epoch: 0,
        transactional: true,
        control: false,
    };
    transaction(&engine, case, &cluster, producer, ABORTED_VALUE, false);
    transaction(&engine, case, &cluster, producer, COMMITTED_VALUE, true);

    // One record per batch, so the aborted record, each marker and the committed record is
    // a window of its own: the position has to move through every one of them.
    let created = create_mapping(
        &router,
        &json!({
            "FunctionName": function,
            "EventSourceArn": cluster,
            "Topics": [TOPIC],
            "StartingPosition": "TRIM_HORIZON",
            "BatchSize": 1,
            "AmazonManagedKafkaEventSourceConfig": { "ConsumerGroupId": GROUP },
            "Enabled": true,
        }),
    )
    .await;
    assert_eq!(
        created.status,
        202,
        "case {case:?}: expected CreateEventSourceMapping to answer 202, got {} {}",
        created.status,
        created.text()
    );
    let uuid = created
        .string("UUID")
        .unwrap_or_else(|| panic!("case {case:?}: no UUID in {}", created.text()));
    await_mapping_state(&router, case, &uuid, STATE_ENABLED).await;

    // LOOKS AGAIN until the group's committed position is past the whole log: four
    // offsets, of which only one carried a record anybody is handed.
    let committed = awaited_commit(&engine, case, &cluster, 4).await;
    assert_eq!(
        committed,
        Some(4),
        "case {case:?}: expected the mapping to have read past the aborted record, both \
         markers and the committed record, got {committed:?}; the mapping reports {}",
        get_mapping(&router, &uuid).await.text()
    );
    // And what it handed over was the committed record, and nothing else: the handler
    // reports every batch it is given on a line of its own, read here out of the
    // function's own log group.
    let group = format!("/aws/lambda/{function}");
    let lines = logged(&router, case, &group).await;
    let handed: Vec<String> = lines
        .iter()
        .filter_map(|line| line.split_once(HANDED_MARKER).map(|(_, rest)| rest.trim().to_string()))
        .collect();
    assert_eq!(
        handed,
        vec![COMMITTED_VALUE.to_string()],
        "case {case:?}: expected one batch holding the committed record alone, got \
         {handed:?} out of {lines:?}"
    );
}

/// Watch the function's log group until its handler has reported a batch, answering the
/// lines it had written.
///
/// The whole chain is waited for TOGETHER — the poller taking the batch, the invocation
/// running and its log reaching the group — because a reader that stopped at the first of
/// them would be reading an instant of its own choosing in the middle of it.
async fn logged(
    router: &::smithy_server_runtime::MultiServiceRouter,
    case: &str,
    group: &str,
) -> Vec<String> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let lines = crate::sqs_event_source_log_delivery::logged_lines(router, case, group).await;
        if lines.iter().any(|line| line.contains(HANDED_MARKER)) {
            return lines;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the handler to report a batch in {group:?}, got {lines:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Write one record inside a transaction of `producer` and end that transaction, through
/// the data plane of the service that OWNS the log — the same append and the same marker a
/// `Produce` and an `EndTxn` on the cluster's bootstrap address write.
fn transaction(
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    cluster_arn: &str,
    producer: RecordProducer,
    value: &str,
    committed: bool,
) {
    let clusters = kafka::service_state(&**engine)
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

/// The offset the group has committed for the topic's one partition, read the way a client
/// reads it: through the coordinator's own `OffsetFetch`.
fn committed_offset(engine: &Arc<engine::InMemoryEngine>, cluster_arn: &str) -> Option<i64> {
    let clusters = kafka::service_state(&**engine).expect("the kafka service must be mounted");
    let store = clusters.lock();
    let held = store
        .consumer_groups
        .get(&(cluster_arn.to_string(), GROUP.to_string()))?;
    let request = OffsetFetchRequest::default()
        .with_group_id(GroupId(StrBytes::from_static_str(GROUP)))
        .with_topics(Some(vec![OffsetFetchRequestTopic::default()
            .with_name(TopicName(StrBytes::from_static_str(TOPIC)))
            .with_partition_indexes(vec![0])]));
    held.fetch_offsets(&request)
        .topics
        .first()
        .and_then(|topic| topic.partitions.first())
        .map(|partition| partition.committed_offset)
}

/// Wait until the group has committed `wanted` for that partition, answering what it had
/// committed.
async fn awaited_commit(
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    cluster_arn: &str,
    wanted: i64,
) -> Option<i64> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let held = committed_offset(engine, cluster_arn);
        if held == Some(wanted) {
            return held;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {GROUP:?} to have committed {wanted} for {TOPIC}-0, it \
             had committed {held:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}
