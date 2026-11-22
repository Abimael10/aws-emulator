// SPDX-License-Identifier: CC0-1.0

//! How large a record a cluster's brokers take, driven over the published bootstrap
//! address.
//!
//! RULE under test: a produce whose record batch is larger than the limit in force for
//! the topic is refused with `MESSAGE_TOO_LARGE` and appends nothing, while a batch under
//! that limit is appended and read back. The limit in force is the TOPIC's own
//! `max.message.bytes` ("The largest record batch size allowed by Kafka", Apache Kafka's
//! topic-level configuration), which OVERRIDES the broker's `message.max.bytes` — that
//! setting is documented as the default a topic's own value replaces — so a topic that
//! narrows the limit refuses a batch the default would have taken, and one that raises it
//! takes a batch the default would have refused.
//!
//! The rows differ along what the rule generalises over: the limit the topic sets (a few
//! hundred bytes, a few kilobytes, none at all, and one above the broker's default),
//! whether the batch is over the limit in ONE record or in the SUM of several, and the
//! record that sits just under it.
//!
//! Every assertion goes over a real socket the core opened for the cluster, dialled at
//! the address `GetBootstrapBrokers` published.

use ::bytes::{Bytes, BytesMut};
use ::kafka_protocol::messages::{
    produce_request::{PartitionProduceData, TopicProduceData},
    ApiKey, ProduceRequest, ProduceResponse, TopicName,
};
use ::kafka_protocol::protocol::StrBytes;
use ::kafka_protocol::records::{
    Compression, Record, RecordBatchEncoder, RecordEncodeOptions, TimestampType,
};
use ::kafka_protocol::ResponseError;
use serde_json::json;

use crate::broker_data_plane::{epoch_millis, given_topic, Client};
use crate::kafka_harness::{sleep_past_activation, ClusterRequest, Emulator};

/// The broker's own `message.max.bytes`, which is the default a topic that sets no
/// `max.message.bytes` of its own runs with: Apache Kafka's documented default, as the
/// crate states it
/// (`service_com_amazonaws_kafka_kafka::state::broker_plane::MAX_RECORD_BYTES`).
const BROKER_DEFAULT: usize =
    service_com_amazonaws_kafka_kafka::state::broker_plane::MAX_RECORD_BYTES;

/// One row: the case, the topic, the `max.message.bytes` it sets (`None` for a topic that
/// sets none), and the limit that is then in force.
struct Row {
    case: &'static str,
    topic: &'static str,
    set: Option<usize>,
    in_force: usize,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a topic that takes a few hundred bytes",
            topic: "size-512",
            set: Some(512),
            in_force: 512,
        },
        Row {
            case: "a topic that takes a few kilobytes",
            topic: "size-4096",
            set: Some(4_096),
            in_force: 4_096,
        },
        Row {
            // A topic that sets none runs Apache Kafka's documented default, which is the
            // broker's own `message.max.bytes`.
            case: "a topic that sets no limit of its own",
            topic: "size-default",
            set: None,
            in_force: BROKER_DEFAULT,
        },
        Row {
            // A topic may RAISE the limit past the broker's default: the broker setting is
            // the default its own overrides.
            case: "a topic that raises the limit above the broker's default",
            topic: "size-above-broker",
            set: Some(BROKER_DEFAULT + 1_000_000),
            in_force: BROKER_DEFAULT + 1_000_000,
        },
    ]
}

/// `Produce` one record batch to one partition, answering the protocol error code the
/// broker returned and the base offset it assigned.
///
/// Written here rather than taken from the shared client because this suite is about the
/// produce the broker REFUSES, and the shared helper asserts the produce was accepted.
async fn produce(
    client: &mut Client,
    case: &str,
    topic: &str,
    values: &[Vec<u8>],
) -> (i16, i64) {
    let batch: Vec<Record> = values
        .iter()
        .enumerate()
        .map(|(index, value)| Record {
            transactional: false,
            control: false,
            partition_leader_epoch: 0,
            producer_id: -1,
            producer_epoch: -1,
            timestamp_type: TimestampType::Creation,
            offset: index as i64,
            sequence: index as i32,
            // The instant the producer stamps the record at, which is what the topic's
            // own retention ages a record out by: a record stamped at the epoch is one
            // every retention window has already left behind.
            timestamp: epoch_millis(),
            key: None,
            value: Some(Bytes::from(value.clone())),
            headers: Default::default(),
            delete_horizon: false,
        })
        .collect();
    let mut encoded = BytesMut::new();
    RecordBatchEncoder::encode(
        &mut encoded,
        batch.iter(),
        &RecordEncodeOptions {
            version: 2,
            compression: Compression::None,
        },
    )
    .unwrap_or_else(|error| panic!("case {case:?}: the batch must encode: {error}"));
    let answered: ProduceResponse = client
        .call(
            case,
            ApiKey::Produce,
            9,
            &ProduceRequest::default()
                .with_acks(1)
                .with_timeout_ms(1_000)
                .with_topic_data(vec![TopicProduceData::default()
                    .with_name(TopicName(StrBytes::from_string(topic.to_string())))
                    .with_partition_data(vec![PartitionProduceData::default()
                        .with_index(0)
                        .with_records(Some(encoded.freeze()))])]),
        )
        .await;
    let partition = answered
        .responses
        .first()
        .and_then(|topic| topic.partition_responses.first())
        .unwrap_or_else(|| panic!("case {case:?}: Produce reported no partition"))
        .clone();
    (partition.error_code, partition.base_offset)
}

/// A value of exactly `bytes` bytes, which is what makes the batch's size the row's
/// subject.
fn value_of(bytes: usize) -> Vec<u8> {
    vec![b'r'; bytes]
}

#[tokio::test]
async fn a_batch_above_the_topics_max_message_bytes_is_refused_and_one_under_it_round_trips() {
    let emulator = Emulator::new();
    let cluster = emulator
        .create_ok(&ClusterRequest {
            client_broker: Some("PLAINTEXT"),
            ..ClusterRequest::named("record-size-limit")
        })
        .await;
    sleep_past_activation();
    let address = emulator
        .published_bootstrap(&cluster, "bootstrapBrokerString", "record size")
        .await;

    for row in rows() {
        let case = row.case;
        let configs = row
            .set
            .map(|limit| json!({ "max.message.bytes": limit.to_string() }));
        given_topic(&emulator, &cluster, row.topic, 1, configs).await;
        let mut client = Client::plaintext(case, &address).await;
        client.api_versions(case).await;

        // One record past the limit: refused, and the log keeps nothing.
        let over = value_of(row.in_force + 1);
        let (refused, _offset) = produce(&mut client, case, row.topic, &[over]).await;
        assert_eq!(
            refused,
            ResponseError::MessageTooLarge.code(),
            "case {case:?}: a record of {} bytes must be refused MESSAGE_TOO_LARGE on a topic \
             taking {}, got error {refused}",
            row.in_force + 1,
            row.in_force
        );
        let held = client.fetch(case, row.topic, 0, 0).await;
        assert!(
            held.is_empty(),
            "case {case:?}: a refused produce must append nothing, got {} record(s)",
            held.len()
        );

        // The SUM of a batch is what the limit is read against, so several records that
        // together pass it are refused too — on the row whose limit leaves room for them.
        if row.in_force >= 8 {
            let half = row.in_force / 2;
            let batch = vec![value_of(half), value_of(half), value_of(half)];
            let (refused_batch, _offset) = produce(&mut client, case, row.topic, &batch).await;
            assert_eq!(
                refused_batch,
                ResponseError::MessageTooLarge.code(),
                "case {case:?}: a batch of {} bytes must be refused MESSAGE_TOO_LARGE on a \
                 topic taking {}, got error {refused_batch}",
                half * 3,
                row.in_force
            );
        }

        // And a record just under the limit round-trips: the refusal is the limit's, not a
        // produce this endpoint cannot serve.
        let under = value_of(row.in_force - 1);
        let (accepted, base) = produce(
            &mut client,
            case,
            row.topic,
            std::slice::from_ref(&under),
        )
        .await;
        assert_eq!(
            accepted, 0,
            "case {case:?}: a record of {} bytes must be appended on a topic taking {}, got \
             error {accepted}",
            row.in_force - 1,
            row.in_force
        );
        assert_eq!(
            base, 0,
            "case {case:?}: the appended record must take the first offset of the log, got \
             {base}"
        );
        let read = client.fetch(case, row.topic, 0, 0).await;
        let lengths: Vec<usize> = read.iter().map(|(_, _, value)| value.len()).collect();
        assert_eq!(
            lengths,
            vec![under.len()],
            "case {case:?}: the record under the limit must be read back whole, got {lengths:?}"
        );
    }
}
