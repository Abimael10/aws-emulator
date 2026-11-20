// SPDX-License-Identifier: CC0-1.0

//! What a produce SUBMITS and what a fetch HANDS BACK, over the published bootstrap
//! address.
//!
//! RULE under test: a record is kept and handed back as the producer wrote it — its key,
//! its value, its headers in the order and the bytes it wrote them (repeated keys, an
//! empty value and an absent value included, KIP-82) and the timestamp it stated —
//! whatever CODEC the batch travels in; a batch whose bytes the broker cannot read is
//! refused as corrupt and appends nothing; and a topic that names a codec of its own
//! hands its records back in that codec, while one that leaves the codec to the producer
//! hands them back in the one they arrived under.
//!
//! The rows span what the rule generalises over: which codec carries the batch (every
//! value `compression.type` admits — the catalog is that configuration's own value list,
//! `uncompressed`, `zstd`, `lz4`, `snappy`, `gzip`, `producer`), which codec the TOPIC
//! keeps, what a record carries (a key or none, a value or none, an empty value, headers
//! or none), and whose clock stamps the record (`CreateTime` against `LogAppendTime`).
//!
//! Every assertion goes over a real socket the core opened for the cluster, dialled at
//! the address `GetBootstrapBrokers` published. The batch a row produces is framed as the
//! protocol frames one; its framing, its checksum and its compression are cross-checked
//! against the protocol implementation's own decoder
//! ([`::kafka_protocol::records::RecordBatchDecoder`]), which reads every field of a
//! record but its repeated headers.

use ::bytes::Bytes;
use ::kafka_protocol::messages::{
    fetch_request::{FetchPartition, FetchTopic},
    produce_request::{PartitionProduceData, TopicProduceData},
    ApiKey, FetchRequest, FetchResponse, ProduceRequest, ProduceResponse, TopicName,
};
use ::kafka_protocol::protocol::StrBytes;
use ::kafka_protocol::records::{
    Compression, Record, RecordBatchDecoder, RecordBatchEncoder, RecordEncodeOptions, TimestampType,
};
use ::kafka_protocol::ResponseError;
use serde_json::json;
use service_com_amazonaws_kafka_kafka::state::broker_plane::{
    records, RecordCodec, RecordHeaders, StoredRecord,
};

use crate::broker_data_plane::Client;
use crate::kafka_harness::{ClusterRequest, Emulator};

/// One row of the codec table: what it is called, the codec the producer chose, the
/// topic's own `compression.type`, and the codec the fetch must answer in.
type CodecRow<'a> = (&'a str, RecordCodec, Option<&'a str>, RecordCodec);

/// The headers every row's first record carries: a key stated TWICE with two different
/// values, a header whose value is empty, one whose value is absent, and one carrying
/// every byte value there is.
fn headers() -> RecordHeaders {
    vec![
        ("trace".to_string(), Some(b"one".to_vec())),
        ("trace".to_string(), Some(b"two".to_vec())),
        ("empty".to_string(), Some(Vec::new())),
        ("absent".to_string(), None),
        (
            "binary".to_string(),
            Some((0..=u8::MAX).collect::<Vec<u8>>()),
        ),
    ]
}

/// The records every row produces: one carrying a key, a multi-kilobyte value and the
/// headers above, one with no key at all, one tombstone (an absent value) and one whose
/// value is empty.
fn submitted(base_timestamp: i64) -> Vec<StoredRecord> {
    vec![
        StoredRecord {
            offset: 0,
            timestamp_ms: base_timestamp,
            log_append_time: false,
            key: Some(b"key-0".to_vec()),
            value: Some(vec![b'v'; 2_400]),
            headers: headers(),
            codec: RecordCodec::None,
        },
        StoredRecord {
            offset: 1,
            timestamp_ms: base_timestamp + 500,
            log_append_time: false,
            key: None,
            value: Some(b"no key".to_vec()),
            headers: Vec::new(),
            codec: RecordCodec::None,
        },
        StoredRecord {
            offset: 2,
            timestamp_ms: base_timestamp + 1_000,
            log_append_time: false,
            key: Some(b"tombstone".to_vec()),
            value: None,
            headers: vec![("only".to_string(), Some(b"one".to_vec()))],
            codec: RecordCodec::None,
        },
        StoredRecord {
            offset: 3,
            timestamp_ms: base_timestamp + 1_500,
            log_append_time: false,
            key: Some(Vec::new()),
            value: Some(Vec::new()),
            headers: Vec::new(),
            codec: RecordCodec::None,
        },
    ]
}

/// The budget a row reads an answered batch under: the same ceiling the broker reads a
/// produce's payload under, which no row of this suite comes near.
fn budget() -> usize {
    records::MAX_DECOMPRESSED_BYTES
}

/// RULE: every codec a producer may choose round-trips, with every field of every record
/// kept — and the codec a batch is handed back in is the TOPIC's where the topic names
/// one, the producer's where it does not.
#[tokio::test]
async fn every_codec_round_trips_with_headers_timestamps_and_the_topic_s_own_codec() {
    let emulator = Emulator::new();
    let cluster = given_plaintext_cluster(&emulator, "codec-round-trip").await;
    let authority = plaintext_address(&emulator, &cluster, "codecs").await;

    // (the codec the producer chose, the topic's own `compression.type`, the codec the
    // fetch must answer in). The topic values are that configuration's own value list.
    let rows: &[CodecRow<'_>] = &[
        ("uncompressed batch, topic leaves it to the producer", RecordCodec::None, None, RecordCodec::None),
        ("gzip batch, topic leaves it to the producer", RecordCodec::Gzip, Some("producer"), RecordCodec::Gzip),
        ("snappy batch, topic leaves it to the producer", RecordCodec::Snappy, Some("producer"), RecordCodec::Snappy),
        ("lz4 batch, topic leaves it to the producer", RecordCodec::Lz4, Some("producer"), RecordCodec::Lz4),
        ("zstd batch, topic leaves it to the producer", RecordCodec::Zstd, Some("producer"), RecordCodec::Zstd),
        ("uncompressed batch, topic keeps zstd", RecordCodec::None, Some("zstd"), RecordCodec::Zstd),
        ("gzip batch, topic keeps lz4", RecordCodec::Gzip, Some("lz4"), RecordCodec::Lz4),
        ("zstd batch, topic keeps snappy", RecordCodec::Zstd, Some("snappy"), RecordCodec::Snappy),
        ("snappy batch, topic keeps gzip", RecordCodec::Snappy, Some("gzip"), RecordCodec::Gzip),
        ("lz4 batch, topic keeps them uncompressed", RecordCodec::Lz4, Some("uncompressed"), RecordCodec::None),
    ];
    for (index, (case, produced_with, topic_codec, answered_with)) in rows.iter().enumerate() {
        let topic = format!("codec-{index}");
        given_topic(&emulator, &cluster, &topic, 1, *topic_codec, None).await;
        let mut client = Client::plaintext(case, &authority).await;
        client.api_versions(case).await;

        let base_timestamp = 1_700_000_000_000;
        let sent = submitted(base_timestamp);
        let payload = records::encode(&sent, *produced_with)
            .unwrap_or_else(|| panic!("case {case:?}: the batch must encode"));
        let acknowledged = produce(&mut client, case, &topic, 0, payload).await;
        assert_eq!(
            (acknowledged.error_code, acknowledged.base_offset),
            (0, 0),
            "case {case:?}: the produce must be acknowledged at offset 0, got error {} at {}",
            acknowledged.error_code,
            acknowledged.base_offset
        );
        // A topic that keeps the producer's own timestamps states no append time at all.
        assert_eq!(
            acknowledged.log_append_time_ms, -1,
            "case {case:?}: a CreateTime topic must state no append time, got {}",
            acknowledged.log_append_time_ms
        );

        let answered = fetched(&mut client, case, &topic, 0, 0).await;
        let read = records::decode(&answered, &mut budget())
            .unwrap_or_else(|| panic!("case {case:?}: the answered batch must decode"));
        let codec = read
            .first()
            .map(|record| record.codec)
            .unwrap_or(RecordCodec::None);
        assert_eq!(
            codec, *answered_with,
            "case {case:?}: the fetch must answer in {answered_with:?}, got {codec:?}"
        );
        assert_eq!(
            read.len(),
            sent.len(),
            "case {case:?}: expected {} records, got {}",
            sent.len(),
            read.len()
        );
        for (expected, got) in sent.iter().zip(read.iter()) {
            assert_eq!(
                (&got.key, &got.value, got.timestamp_ms),
                (&expected.key, &expected.value, expected.timestamp_ms),
                "case {case:?}: the record must come back byte for byte, got {got:?}"
            );
            assert_eq!(
                got.headers, expected.headers,
                "case {case:?}: the headers must come back in order and byte for byte, \
                 got {:?}",
                got.headers
            );
        }
        // The framing and the checksum of that batch, read by the protocol
        // implementation's own decoder: an independent reading of the bytes. It is taken
        // with no codec of its own, so it reads the rows a topic keeps uncompressed.
        if *answered_with == RecordCodec::None {
            let mut raw = answered.clone();
            let independent = RecordBatchDecoder::decode(&mut raw)
                .unwrap_or_else(|error| panic!("case {case:?}: the batch must decode: {error}"));
            assert_eq!(
                independent.records.len(),
                sent.len(),
                "case {case:?}: the protocol's own decoder must read {} records, got {}",
                sent.len(),
                independent.records.len()
            );
        }
    }
}

/// RULE, read from the other side: a batch another implementation WROTE is read back
/// field for field — the record layer here is the published format, not a form of its own.
#[tokio::test]
async fn a_batch_the_protocol_s_own_encoder_wrote_is_read_field_for_field() {
    let case = "a batch the protocol implementation encoded";
    let emulator = Emulator::new();
    let cluster = given_plaintext_cluster(&emulator, "codec-foreign").await;
    let authority = plaintext_address(&emulator, &cluster, case).await;
    let topic = "foreign";
    given_topic(&emulator, &cluster, topic, 1, None, None).await;
    let mut client = Client::plaintext(case, &authority).await;
    client.api_versions(case).await;

    let timestamp = 1_700_000_000_123;
    let batch = [Record {
        transactional: false,
        control: false,
        partition_leader_epoch: 0,
        producer_id: -1,
        producer_epoch: -1,
        timestamp_type: TimestampType::Creation,
        offset: 0,
        sequence: 0,
        timestamp,
        key: Some(Bytes::from_static(b"foreign-key")),
        value: Some(Bytes::from_static(b"foreign-value")),
        headers: [(
            StrBytes::from_static_str("written-elsewhere"),
            Some(Bytes::from_static(b"yes")),
        )]
        .into_iter()
        .collect(),
        delete_horizon: false,
    }];
    let mut encoded = ::bytes::BytesMut::new();
    RecordBatchEncoder::encode(
        &mut encoded,
        batch.iter(),
        &RecordEncodeOptions {
            version: 2,
            compression: Compression::None,
        },
    )
    .unwrap_or_else(|error| panic!("case {case:?}: the batch must encode: {error}"));
    let acknowledged = produce(&mut client, case, topic, 0, encoded.freeze()).await;
    assert_eq!(
        acknowledged.error_code, 0,
        "case {case:?}: the produce must be acknowledged, got error {}",
        acknowledged.error_code
    );

    let answered = fetched(&mut client, case, topic, 0, 0).await;
    let read = records::decode(&answered, &mut budget()).expect("the answered batch decodes");
    let got = read
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: the fetch must answer the record"));
    assert_eq!(
        (
            got.key.clone(),
            got.value.clone(),
            got.timestamp_ms,
            got.headers.clone()
        ),
        (
            Some(b"foreign-key".to_vec()),
            Some(b"foreign-value".to_vec()),
            timestamp,
            vec![("written-elsewhere".to_string(), Some(b"yes".to_vec()))]
        ),
        "case {case:?}: the record another encoder wrote must read back field for field, \
         got {got:?}"
    );
}

/// RULE: a topic that stamps its records itself (`message.timestamp.type`
/// `LogAppendTime`) replaces the producer's timestamp with the instant it appended the
/// record, acknowledges that instant and hands the record back carrying it.
#[tokio::test]
async fn a_log_append_time_topic_stamps_its_own_records() {
    let case = "a LogAppendTime topic";
    let emulator = Emulator::new();
    let cluster = given_plaintext_cluster(&emulator, "codec-stamp").await;
    let authority = plaintext_address(&emulator, &cluster, case).await;
    let topic = "stamped";
    given_topic(
        &emulator,
        &cluster,
        topic,
        1,
        None,
        Some("LogAppendTime"),
    )
    .await;
    let mut client = Client::plaintext(case, &authority).await;
    client.api_versions(case).await;

    // A timestamp the producer states well in the past, so the broker's own stamp cannot
    // be mistaken for it.
    let stated = 1_600_000_000_000;
    let before = epoch_millis();
    let sent = submitted(stated);
    let payload = records::encode(&sent, RecordCodec::None).expect("the batch encodes");
    let acknowledged = produce(&mut client, case, topic, 0, payload).await;
    let after = epoch_millis();
    assert_eq!(
        acknowledged.error_code, 0,
        "case {case:?}: the produce must be acknowledged, got error {}",
        acknowledged.error_code
    );
    assert!(
        (before..=after).contains(&acknowledged.log_append_time_ms),
        "case {case:?}: the acknowledgement must carry the instant the broker appended \
         the batch, got {}",
        acknowledged.log_append_time_ms
    );

    let answered = fetched(&mut client, case, topic, 0, 0).await;
    let read = records::decode(&answered, &mut budget()).expect("the answered batch decodes");
    assert_eq!(
        read.len(),
        sent.len(),
        "case {case:?}: every record must come back, got {}",
        read.len()
    );
    for record in &read {
        assert!(
            (before..=after).contains(&record.timestamp_ms),
            "case {case:?}: the record must carry the broker's own stamp, got {}",
            record.timestamp_ms
        );
        assert_ne!(
            record.timestamp_ms, stated,
            "case {case:?}: a LogAppendTime topic must not keep the producer's timestamp"
        );
    }
}

/// RULE: a records payload the broker cannot read is refused `CORRUPT_MESSAGE` and
/// appends nothing, so the partition's end offset does not move.
#[tokio::test]
async fn an_unreadable_batch_is_refused_and_appends_nothing() {
    let emulator = Emulator::new();
    let cluster = given_plaintext_cluster(&emulator, "codec-corrupt").await;
    let authority = plaintext_address(&emulator, &cluster, "corrupt").await;
    let topic = "corrupt";
    given_topic(&emulator, &cluster, topic, 1, None, None).await;
    let mut client = Client::plaintext("corrupt", &authority).await;
    client.api_versions("corrupt").await;

    // One good batch first, so the rows below have an end offset that must not move.
    let sent = submitted(1_700_000_000_000);
    let good = records::encode(&sent, RecordCodec::None).expect("the batch encodes");
    produce(&mut client, "a readable batch", topic, 0, good.clone()).await;

    let truncated = good.slice(0..good.len() - 4);
    let mut broken_checksum = good.to_vec();
    // The byte after the magic opens the batch's CRC, so flipping it leaves a batch
    // whose stated checksum is not its own.
    let crc_byte = 8 + 4 + 4 + 1;
    broken_checksum[crc_byte] ^= 0xff;
    // A batch that STATES more bytes than it carries, and one that states more records
    // than its bytes hold: what an oversized claim must not make the broker read or
    // allocate for.
    let mut overstated_length = good.to_vec();
    overstated_length[8..12].copy_from_slice(&i32::MAX.to_be_bytes());
    let mut overstated_count = good.to_vec();
    let count_byte = 8 + 4 + 4 + 1 + 4 + 2 + 4 + 8 + 8 + 8 + 2 + 4;
    overstated_count[count_byte..count_byte + 4].copy_from_slice(&i32::MAX.to_be_bytes());
    // A batch whose records are compressed and EXPAND past what one payload may hold:
    // what a decoder must refuse while it runs rather than after it has held the bytes.
    let bomb = records::encode(
        &[StoredRecord {
            offset: 0,
            timestamp_ms: 1_700_000_000_000,
            log_append_time: false,
            key: None,
            // Highly compressible, and far past the bytes of records one payload holds.
            value: Some(vec![0u8; 16 * 1024 * 1024]),
            headers: Vec::new(),
            codec: RecordCodec::Gzip,
        }],
        RecordCodec::Gzip,
    )
    .expect("the batch encodes");
    assert!(
        bomb.len() < 1024 * 1024,
        "the expansion case must be small on the wire, it is {} bytes",
        bomb.len()
    );
    let rows: &[(&str, Bytes)] = &[
        ("random bytes", Bytes::from_static(b"not a record batch at all")),
        ("a truncated batch", truncated),
        ("a batch whose checksum is not its own", Bytes::from(broken_checksum)),
        (
            "a batch stating more bytes than it carries",
            Bytes::from(overstated_length),
        ),
        (
            "a batch stating more records than its bytes hold",
            Bytes::from(overstated_count),
        ),
        ("one byte", Bytes::from_static(b"\x02")),
        ("a batch that expands past what a payload may hold", bomb),
    ];
    for (case, payload) in rows {
        let refused = produce(&mut client, case, topic, 0, payload.clone()).await;
        assert_eq!(
            refused.error_code,
            ResponseError::CorruptMessage.code(),
            "case {case:?}: an unreadable batch must be refused CORRUPT_MESSAGE, got {}",
            refused.error_code
        );
        let answered = fetched(&mut client, case, topic, 0, 0).await;
        let read = records::decode(&answered, &mut budget())
            .unwrap_or_else(|| panic!("case {case:?}: the answered batch must decode"));
        assert_eq!(
            read.len(),
            sent.len(),
            "case {case:?}: nothing must have been appended, the log holds {} records",
            read.len()
        );
    }
}

/// `Produce` one batch to one partition, answering the partition's own result.
async fn produce(
    client: &mut Client,
    case: &str,
    topic: &str,
    partition: i32,
    payload: Bytes,
) -> ::kafka_protocol::messages::produce_response::PartitionProduceResponse {
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
                        .with_index(partition)
                        .with_records(Some(payload))])]),
        )
        .await;
    answered
        .responses
        .first()
        .and_then(|topic| topic.partition_responses.first())
        .unwrap_or_else(|| panic!("case {case:?}: Produce reported no partition"))
        .clone()
}

/// The records payload a `Fetch` of one partition answers, as it stands on the wire.
async fn fetched(
    client: &mut Client,
    case: &str,
    topic: &str,
    partition: i32,
    offset: i64,
) -> Bytes {
    let answered: FetchResponse = client
        .call(
            case,
            ApiKey::Fetch,
            11,
            &FetchRequest::default()
                .with_max_wait_ms(100)
                .with_min_bytes(0)
                .with_max_bytes(1_048_576)
                .with_topics(vec![FetchTopic::default()
                    .with_topic(TopicName(StrBytes::from_string(topic.to_string())))
                    .with_partitions(vec![FetchPartition::default()
                        .with_partition(partition)
                        .with_fetch_offset(offset)
                        .with_partition_max_bytes(1_048_576)])]),
        )
        .await;
    let partition_data = answered
        .responses
        .first()
        .and_then(|topic| topic.partitions.first())
        .unwrap_or_else(|| panic!("case {case:?}: Fetch reported no partition"))
        .clone();
    assert_eq!(
        partition_data.error_code, 0,
        "case {case:?}: Fetch must be answered, got error {}",
        partition_data.error_code
    );
    partition_data.records.clone().unwrap_or_default()
}

/// A cluster whose brokers are reached in the clear, so this suite dials one socket
/// without a credential.
async fn given_plaintext_cluster(emulator: &Emulator, name: &str) -> String {
    emulator
        .create_ok(&ClusterRequest {
            client_broker: Some("PLAINTEXT"),
            ..ClusterRequest::named(name)
        })
        .await
}

/// The address a cluster publishes for its plaintext path, once it publishes one.
async fn plaintext_address(emulator: &Emulator, cluster: &str, case: &str) -> String {
    emulator
        .published_bootstrap(cluster, "bootstrapBrokerString", case)
        .await
}

/// A topic of the cluster, created through the control plane with the codec and the
/// timestamp type a row needs.
async fn given_topic(
    emulator: &Emulator,
    cluster: &str,
    name: &str,
    partitions: i32,
    codec: Option<&str>,
    timestamp_type: Option<&str>,
) {
    let mut configs = serde_json::Map::new();
    // The records a row produces carry timestamps of their own, so the topic keeps every
    // record for as long as the suite runs: `retention.ms` -1 is Apache Kafka's "no
    // limit", which is what makes a row's assertions about the log independent of when
    // the row produced.
    configs.insert("retention.ms".to_string(), json!("-1"));
    if let Some(codec) = codec {
        configs.insert("compression.type".to_string(), json!(codec));
    }
    if let Some(timestamp_type) = timestamp_type {
        configs.insert("message.timestamp.type".to_string(), json!(timestamp_type));
    }
    let mut body = json!({
        "topicName": name,
        "partitionCount": partitions,
        "replicationFactor": 1,
    });
    // The configuration is never empty: every row sets `retention.ms`, and the codec and
    // the timestamp type a row needs sit beside it.
    let encoded =
        ::aws_smithy_types::base64::encode(serde_json::Value::Object(configs).to_string());
    body["configs"] = json!(encoded);
    let reply = emulator
        .call(
            "POST",
            &format!("/v1/clusters/{}/topics", crate::kafka_harness::pct(cluster)),
            Some(body),
        )
        .await;
    assert_eq!(
        reply.status, 200,
        "the topic {name} must be created, got {} {}",
        reply.status,
        reply.error_message()
    );
}

/// The current instant in epoch milliseconds, the unit a record's timestamp is in.
fn epoch_millis() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the clock is past the epoch")
            .as_millis(),
    )
    .expect("the instant fits in an i64")
}
