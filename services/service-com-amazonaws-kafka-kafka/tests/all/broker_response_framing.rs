// SPDX-License-Identifier: CC0-1.0

//! How the broker WRITES its answers: one request frame, one write.
//!
//! RULE under test: the broker answers a request frame with its length and the response
//! body that length introduces in ONE write, so the first read of the answer carries
//! both. A client reads the four-byte length before it reads anything else, so a write
//! that carries only the length gives the client nothing to act on: the body is a second
//! segment the kernel holds back until the first is acknowledged (Nagle against the
//! client's delayed acknowledgement), and the client waits tens of milliseconds for an
//! answer the broker had already computed — on every request of every connection.
//!
//! The rows span what the rule generalises over: which API answers (the negotiation, the
//! cluster metadata, topic administration, a produce and a fetch) and how large its
//! response is, since the rule is about the WRITE rather than about any one answer.
//!
//! Every row goes over a real socket the core opened for the cluster, dialled at the
//! address `GetBootstrapBrokers` published, and reads the answer with a SINGLE read: the
//! length and the body it introduces have to arrive together.

use ::bytes::{Bytes, BytesMut};
use ::kafka_protocol::messages::{
    create_topics_request::CreatableTopic,
    fetch_request::{FetchPartition, FetchTopic},
    produce_request::{PartitionProduceData, TopicProduceData},
    ApiKey, ApiVersionsRequest, CreateTopicsRequest, FetchRequest, ListOffsetsRequest,
    MetadataRequest, ProduceRequest, RequestHeader, TopicName,
};
use ::kafka_protocol::protocol::{Encodable, StrBytes};
use ::kafka_protocol::records::{
    Compression, Record, RecordBatchEncoder, RecordEncodeOptions, TimestampType,
};
use ::tokio::io::{AsyncReadExt, AsyncWriteExt};
use ::tokio::net::TcpStream;

use crate::kafka_harness::{ClusterRequest, Emulator};

/// The client every row below identifies itself with.
const CLIENT_ID: &str = "framing-client";

/// The topic the administration, produce and fetch rows address.
const TOPIC: &str = "framing-topic";

/// RULE: every answer's length and body arrive in one read, because the broker writes
/// them as one write.
#[tokio::test]
async fn every_answer_carries_its_length_and_its_body_in_one_write() {
    let case = "an answer read with a single read";
    let emulator = Emulator::new();
    let cluster = emulator
        .create_ok(&ClusterRequest {
            client_broker: Some("PLAINTEXT"),
            unauthenticated: Some(true),
            ..ClusterRequest::named("framing")
        })
        .await;
    let authority = emulator
        .published_bootstrap(&cluster, "bootstrapBrokerString", case)
        .await;
    let mut connection = dial(&authority, case).await;

    // The rows span the APIs a client drives a broker with, in the order a client sends
    // them: each one's answer is read with a single read of the socket.
    let rows: Vec<(&str, ApiKey, i16, Vec<u8>)> = vec![
        (
            "the version negotiation",
            ApiKey::ApiVersions,
            3,
            encoded(
                ApiKey::ApiVersions,
                3,
                &ApiVersionsRequest::default()
                    .with_client_software_name(StrBytes::from_static_str(CLIENT_ID))
                    .with_client_software_version(StrBytes::from_static_str("1")),
                case,
            ),
        ),
        (
            "topic administration",
            ApiKey::CreateTopics,
            5,
            encoded(
                ApiKey::CreateTopics,
                5,
                &CreateTopicsRequest::default()
                    .with_timeout_ms(1_000)
                    .with_topics(vec![CreatableTopic::default()
                        .with_name(TopicName(StrBytes::from_static_str(TOPIC)))
                        .with_num_partitions(2)
                        .with_replication_factor(1)]),
                case,
            ),
        ),
        (
            "the cluster metadata",
            ApiKey::Metadata,
            9,
            encoded(
                ApiKey::Metadata,
                9,
                &MetadataRequest::default().with_topics(None),
                case,
            ),
        ),
        (
            "a produce",
            ApiKey::Produce,
            9,
            encoded(
                ApiKey::Produce,
                9,
                &ProduceRequest::default()
                    .with_acks(1)
                    .with_timeout_ms(1_000)
                    .with_topic_data(vec![TopicProduceData::default()
                        .with_name(TopicName(StrBytes::from_static_str(TOPIC)))
                        .with_partition_data(vec![PartitionProduceData::default()
                            .with_index(0)
                            .with_records(Some(one_record(case)))])]),
                case,
            ),
        ),
        (
            "a fetch",
            ApiKey::Fetch,
            11,
            encoded(
                ApiKey::Fetch,
                11,
                &FetchRequest::default()
                    .with_max_wait_ms(100)
                    .with_min_bytes(0)
                    .with_max_bytes(1_048_576)
                    .with_topics(vec![FetchTopic::default()
                        .with_topic(TopicName(StrBytes::from_static_str(TOPIC)))
                        .with_partitions(vec![FetchPartition::default()
                            .with_partition(0)
                            .with_fetch_offset(0)
                            .with_partition_max_bytes(1_048_576)])]),
                case,
            ),
        ),
        (
            "the log's bounds",
            ApiKey::ListOffsets,
            5,
            encoded(
                ApiKey::ListOffsets,
                5,
                &ListOffsetsRequest::default().with_topics(Vec::new()),
                case,
            ),
        ),
    ];

    for (row, key, version, frame) in rows {
        let length = u32::try_from(frame.len()).expect("a request frame fits in a u32");
        let mut request = Vec::with_capacity(frame.len() + 4);
        request.extend_from_slice(&length.to_be_bytes());
        request.extend_from_slice(&frame);
        connection
            .write_all(&request)
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: row {row:?}: the request must be written: {error}"));

        // ONE read. A broker that wrote the length on its own would have given the
        // client four bytes and nothing to act on.
        let mut buffer = vec![0u8; 64 * 1024];
        let read = connection
            .read(&mut buffer)
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: row {row:?}: the answer must be read: {error}"));
        assert!(
            read > 4,
            "case {case:?}: row {row:?} ({key:?} v{version}): the first read of the answer \
             must carry the length AND the body it introduces, got {read} byte(s)"
        );
        let stated = u32::from_be_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]) as usize;
        assert!(
            stated > 0 && stated < 64 * 1024,
            "case {case:?}: row {row:?}: the answer states a length of {stated} bytes"
        );
        assert_eq!(
            read,
            stated + 4,
            "case {case:?}: row {row:?} ({key:?} v{version}): the whole answer must arrive in \
             that one read, got {read} of {} byte(s)",
            stated + 4
        );
    }
}

/// Dial the published address in the clear, with Nagle off as every Kafka client has it:
/// what is measured here is what the BROKER writes, so the client must not hold its own
/// request back.
async fn dial(authority: &str, case: &str) -> TcpStream {
    let (_host, port) = authority
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("case {case:?}: {authority:?} is not a host:port pair"));
    let port: u16 = port
        .parse()
        .unwrap_or_else(|_| panic!("case {case:?}: {port:?} is not a port"));
    let connection = TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: {authority} must accept a connection: {error}"));
    connection
        .set_nodelay(true)
        .unwrap_or_else(|error| panic!("case {case:?}: the connection must take TCP_NODELAY: {error}"));
    connection
}

/// One request's header and body, encoded as the protocol frames them for this API and
/// version.
fn encoded<Request: Encodable>(
    key: ApiKey,
    version: i16,
    request: &Request,
    case: &str,
) -> Vec<u8> {
    let mut frame = BytesMut::new();
    RequestHeader::default()
        .with_request_api_key(key as i16)
        .with_request_api_version(version)
        .with_correlation_id(1)
        .with_client_id(Some(StrBytes::from_static_str(CLIENT_ID)))
        .encode(&mut frame, key.request_header_version(version))
        .unwrap_or_else(|error| panic!("case {case:?}: the header must encode: {error}"));
    request
        .encode(&mut frame, version)
        .unwrap_or_else(|error| panic!("case {case:?}: the request must encode: {error}"));
    frame.to_vec()
}

/// One record batch holding a single record, as a producer sends it.
fn one_record(case: &str) -> Bytes {
    let records = [Record {
        transactional: false,
        control: false,
        partition_leader_epoch: 0,
        producer_id: -1,
        producer_epoch: -1,
        timestamp_type: TimestampType::Creation,
        offset: 0,
        sequence: 0,
        timestamp: 1_700_000_000_000,
        key: None,
        value: Some(Bytes::from_static(b"one")),
        headers: Default::default(),
        delete_horizon: false,
    }];
    let mut encoded = BytesMut::new();
    RecordBatchEncoder::encode(
        &mut encoded,
        records.iter(),
        &RecordEncodeOptions {
            version: 2,
            compression: Compression::None,
        },
    )
    .unwrap_or_else(|error| panic!("case {case:?}: the batch must encode: {error}"));
    encoded.freeze()
}
