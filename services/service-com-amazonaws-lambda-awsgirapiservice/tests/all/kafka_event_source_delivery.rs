// SPDX-License-Identifier: CC0-1.0

//! The CONSUMER GROUP an event source mapping on an Amazon MSK cluster reads its topic as, as
//! a client of that cluster sees it — on the same four services the delivery suite mounts
//! ([`crate::msk_event_source_mapping_delivery::emulator_on`]), so the group is the one that
//! cluster's own coordinator holds.
//!
//! The RULE asserted here: `AmazonManagedKafkaEventSourceConfig$ConsumerGroupId` is "the
//! identifier for the Kafka consumer group to join", so a mapping that consumes a cluster's
//! topic is a MEMBER of that group while it consumes — a reader of the group is told about
//! that member exactly as about one that joined over the wire, with the subscription it joined
//! with and the partitions it holds — and the position it has read through is the GROUP's
//! committed offset, which is where every other reader of the cluster sees how far the mapping
//! has got. "When false, Lambda pauses polling and invocation" (`Enabled`), and a consumer
//! that has stopped reading is no member of its group any more, so a paused mapping holds no
//! membership while the offsets it committed stay the group's — which is what it resumes from.
//!
//! The delivery itself, and which records a mapping's `FilterCriteria` admit, are asserted in
//! [`crate::msk_event_source_mapping_delivery`]; what is here is the group that delivery is
//! made AS, read through the coordinator's own group reads and its own `OffsetFetch`.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use kafka_protocol::messages::{offset_fetch_request::OffsetFetchRequestTopic, OffsetFetchRequest};
use kafka_protocol::messages::{GroupId, TopicName};
use kafka_protocol::protocol::StrBytes;
use serde_json::json;
use service_com_amazonaws_kafka_kafka::state as kafka;
use service_com_amazonaws_kafka_kafka::state::broker_plane::SubmittedRecord;
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mappings::{
    await_mapping_state, call, create_mapping, create_running_function, get_mapping, LOOK_AGAIN,
    MAPPINGS_URI, STATE_DISABLED, STATE_ENABLED,
};
use crate::msk_event_source_mapping_delivery::{emulator_on, given_cluster, given_topic};

/// How long a wait for work the mapping has to do is given before the suite is a failure. Well
/// above every settle span the chain crosses — the cluster's, the topic's, the function's, the
/// mapping's and the poller's interval — so it fails only when the work was not done at all.
const BUDGET: Duration = Duration::from_secs(60);

/// How long the function below is given to run one batch.
const FUNCTION_TIMEOUT: i64 = 60;

/// The `protocol_type` a group of CONSUMERS runs under, and the assignor its members name:
/// what a reader of the group is told about them (Apache Kafka's `ConsumerProtocol`).
const CONSUMER_PROTOCOL_TYPE: &str = "consumer";
/// See [`CONSUMER_PROTOCOL_TYPE`].
const RANGE_ASSIGNOR: &str = "range";

/// The state a group whose membership is settled reports, and the state an empty one does.
const STABLE: &str = "Stable";
/// See [`STABLE`].
const EMPTY: &str = "Empty";

/// The topic the mapping below consumes, and the group it consumes it as.
const TOPIC: &str = "orders";
/// See [`TOPIC`].
const GROUP: &str = "msk-group-reader-consumers";

/// The handler the mapping's function runs: it accepts the event and reports how many records
/// it was handed, so an invocation succeeds and the pass commits what it read.
const COUNTING_HANDLER: &str = r#"
def lambda_handler(event, context):
    return {"records": sum(len(batch) for batch in event["records"].values())}
"#;

/// One member of the group as a reader of it is told about it.
#[derive(Debug, Clone)]
struct Member {
    /// The client the member goes by, which is what tells one member of a group from another.
    client_id: String,
    /// The subscription it joined with, in the consumer protocol's own bytes.
    subscription: Vec<u8>,
    /// The partitions it holds, likewise.
    assignment: Vec<u8>,
}

/// The group as a reader of it is told about it: the state it is in, the protocol its members
/// agreed on, and one entry per member.
#[derive(Debug, Clone, Default)]
struct Described {
    state: String,
    protocol_type: String,
    protocol: String,
    members: Vec<Member>,
}

/// Produce one record to the mapped topic, through the data plane of the service that OWNS the
/// log — the same append a `Produce` on the cluster's bootstrap address makes (which is what
/// the delivery suite drives over a socket; this suite's subject is the GROUP, so it appends
/// through the store the broker's own produce writes).
///
/// The record carries no timestamp of its own, which is what has the broker stamp it at the
/// instant it appends it.
fn produce(engine: &Arc<engine::InMemoryEngine>, case: &str, cluster_arn: &str, value: &str) {
    let clusters = kafka::service_state(&**engine)
        .unwrap_or_else(|| panic!("case {case:?}: the kafka service must be mounted"));
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
        }],
        SystemTime::now(),
    );
    appended.unwrap_or_else(|refusal| {
        panic!("case {case:?}: producing {value:?} to {TOPIC} was refused: {refusal:?}")
    });
}

/// What the group reports about itself and its members, as a client's `DescribeGroups` reads
/// it out of the coordinator that holds it.
fn described_group(engine: &Arc<engine::InMemoryEngine>, cluster_arn: &str) -> Described {
    let clusters = kafka::service_state(&**engine).expect("the kafka service must be mounted");
    let store = clusters.lock();
    store
        .consumer_groups
        .get(&(cluster_arn.to_string(), GROUP.to_string()))
        .map(|held| Described {
            state: held.state().name().to_string(),
            protocol_type: held.protocol_type().unwrap_or_default().to_string(),
            protocol: held.protocol_name().unwrap_or_default().to_string(),
            members: held
                .described_members()
                .into_iter()
                .map(|member| Member {
                    client_id: member.client_id,
                    subscription: member.metadata.to_vec(),
                    assignment: member.assignment.to_vec(),
                })
                .collect(),
        })
        .unwrap_or_default()
}

/// The offset the group has committed for the topic's one partition, read the way a client
/// reads it: through the coordinator's own `OffsetFetch`.
///
/// `None` while the cluster holds no such group, and the protocol's own "no committed offset"
/// (-1) while the group has committed none for that partition.
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

/// Wait until the group holds `wanted` members, answering what it reported.
async fn awaited_members(
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    cluster_arn: &str,
    wanted: usize,
) -> Described {
    let deadline = Instant::now() + BUDGET;
    loop {
        let reported = described_group(engine, cluster_arn);
        if reported.members.len() == wanted {
            return reported;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the group {GROUP:?} to hold {wanted} member(s), it reported \
             {reported:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
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
            "case {case:?}: expected {GROUP:?} to have committed {wanted} for {TOPIC}-0, it had \
             committed {held:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// `UpdateEventSourceMapping` setting `Enabled`, waited out to the state that setting asks for.
async fn set_enabled(router: &MultiServiceRouter, case: &str, uuid: &str, enabled: bool) {
    let answered = call(
        router,
        "PUT",
        &format!("{MAPPINGS_URI}/{uuid}"),
        &json!({ "Enabled": enabled }),
    )
    .await;
    assert_eq!(
        answered.status,
        202,
        "case {case:?}: expected UpdateEventSourceMapping(Enabled={enabled}) to answer 202, got \
         {} {}",
        answered.status,
        answered.text()
    );
    let wanted = if enabled { STATE_ENABLED } else { STATE_DISABLED };
    await_mapping_state(router, case, uuid, wanted).await;
}

/// RULE: a mapping that consumes a cluster's topic is a real MEMBER of the consumer group it
/// reads as — reported with the client it goes by, the subscription it joined with and the
/// partitions it holds, in a group that is `Stable` on the consumer protocol — and the position
/// it has read through is that group's committed offset; a mapping whose polling is PAUSED
/// holds no membership while the offsets it committed stay the group's.
#[tokio::test]
async fn the_group_a_mapping_consumes_as_reports_its_consumer_and_its_position() {
    let case = "a mapping consuming one topic as a group of its own";
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(&engine);
    let cluster = given_cluster(&router, case, "msk-group-reader").await;
    given_topic(&router, case, &cluster, TOPIC, 1).await;
    let function = "msk-group-reader-fn";
    create_running_function(
        &router,
        case,
        function,
        COUNTING_HANDLER,
        FUNCTION_TIMEOUT,
        &json!({}),
    )
    .await;
    let created = create_mapping(
        &router,
        &json!({
            "FunctionName": function,
            "EventSourceArn": cluster,
            "Topics": [TOPIC],
            "StartingPosition": "TRIM_HORIZON",
            "BatchSize": 10,
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

    // One consumer holds the group: the one this mapping reads as.
    let reported = awaited_members(&engine, case, &cluster, 1).await;
    assert_eq!(
        (
            reported.state.as_str(),
            reported.protocol_type.as_str(),
            reported.protocol.as_str()
        ),
        (STABLE, CONSUMER_PROTOCOL_TYPE, RANGE_ASSIGNOR),
        "case {case:?}: expected a stable group of consumers, got {reported:?}"
    );
    let member = reported
        .members
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the group reported no member"));
    assert!(
        member.client_id.contains(&uuid),
        "case {case:?}: expected the member to go by the mapping it reads for, got {:?}",
        member.client_id
    );
    // The subscription and the assignment are the consumer protocol's own documents, so a
    // reader of the group decodes the topic the consumer subscribed to and the partition it
    // holds out of the bytes themselves.
    for (what, bytes) in [
        ("subscription", &member.subscription),
        ("assignment", &member.assignment),
    ] {
        assert!(
            bytes
                .windows(TOPIC.len())
                .any(|window| window == TOPIC.as_bytes()),
            "case {case:?}: the member's {what} must name {TOPIC:?}, got {bytes:?}"
        );
    }

    // And the position it reads through is the group's: the record produced now is consumed,
    // and the offset past it is what the group has committed.
    produce(&engine, case, &cluster, "the-first-record");
    let committed = awaited_commit(&engine, case, &cluster, 1).await;
    assert_eq!(
        committed,
        Some(1),
        "case {case:?}: expected the group to have committed past the record it consumed, got \
         {committed:?}"
    );
    let read = get_mapping(&router, &uuid).await;
    assert_eq!(
        read.string("LastProcessingResult").as_deref(),
        Some("OK"),
        "case {case:?}: a mapping that delivered its batch must report the attempt, got {}",
        read.text()
    );

    // Polling PAUSED: the consumer stands down, so the group holds no member — and the offset
    // it committed stays the group's, which is where the mapping resumes from.
    set_enabled(&router, case, &uuid, false).await;
    let reported = awaited_members(&engine, case, &cluster, 0).await;
    assert_eq!(
        reported.state.as_str(),
        EMPTY,
        "case {case:?}: a paused mapping holds no membership, so its group must report itself \
         empty, got {reported:?}"
    );
    let committed = committed_offset(&engine, &cluster);
    assert_eq!(
        committed,
        Some(1),
        "case {case:?}: the offsets a consumer committed stay the group's once it stands down, \
         got {committed:?}"
    );
}
