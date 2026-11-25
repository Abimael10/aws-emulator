// SPDX-License-Identifier: CC0-1.0

//! What an event source mapping on an Amazon MSK cluster's TOPIC does with NO client action at
//! all, end to end over the wire, with Amazon MSK and Amazon SQS mounted beside Lambda on ONE
//! engine — so every record is produced where a Kafka client produces it, over the Apache Kafka
//! endpoint the cluster itself published, and every delivery is read back where a client sees
//! it: off the queue the handler forwards to.
//!
//! The RULE asserted over the rows below: once a mapping whose `EventSourceArn` names an MSK
//! cluster is `Enabled`, Lambda reads each partition of each topic the mapping names from the
//! position that mapping carries and INVOKES the mapped function with the records it read
//! ("Lambda reads items from the event source and invokes the function"), as the event a Kafka
//! source gives — `eventSource` `aws:kafka`, the cluster it came from, the brokers its consumer
//! reached, and the records keyed by the topic and partition they came from. Of those records
//! exactly the ones the mapping's `FilterCriteria` admit reach the function, "the filter
//! criteria that determine whether Lambda should process an event", and the position moves past
//! what the pass HANDLED — so the records behind a rejected one are delivered, and the records
//! produced while `Enabled` was false ("Lambda pauses polling and invocation") are delivered
//! once it is true again, from the offset the mapping kept.
//!
//! Every verdict is computed from what the HANDLER was handed: it refuses an event of any other
//! shape, and forwards each record's own decoded value, so a body on the sink queue is a record
//! that really reached a run of the function.
//!
//! The rows differ along what the rule generalises over: how many partitions the topic has, how
//! many records are produced and in what order, the `BatchSize` they are read in, the
//! `StartingPosition` the mapping reads from, whether a record carries a key, whether its value
//! spells a document or plain text, which patterns the criteria carry, and whether polling is
//! paused between two productions.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use bytes::{Bytes, BytesMut};
use kafka_protocol::messages::{
    produce_request::{PartitionProduceData, TopicProduceData},
    ApiKey, ProduceRequest, ProduceResponse, RequestHeader, ResponseHeader, TopicName,
};
use kafka_protocol::protocol::{Decodable, Encodable, StrBytes};
use kafka_protocol::records::{
    Compression, Record, RecordBatchEncoder, RecordEncodeOptions, TimestampType,
};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::event_source_mappings::{
    await_mapping_state, call, create_queue, create_running_function, forwarded_records,
    get_mapping, given_enabled_mapping, router_on, CLOCK_BUDGET, LOOK_AGAIN, MAPPINGS_URI,
    RESULTS_QUEUE_URL, STATE_DISABLED, STATE_ENABLED,
};

/// The Region every call below is signed for, which is the Region every resource lives in.
const REGION: &str = "us-east-1";

/// The Apache Kafka version and broker type every cluster below runs: a version Amazon MSK
/// still takes for a NEW cluster, since each create has to be admitted.
const KAFKA_VERSION: &str = "3.9.x";
/// See [`KAFKA_VERSION`].
const INSTANCE_TYPE: &str = "kafka.m5.large";

/// The client-broker encryption each cluster below offers, so the brokers publish a PLAINTEXT
/// bootstrap address the producer below dials
/// (`smithy_query(query="shape", shape="ClientBroker")`, kafka).
const PLAINTEXT: &str = "PLAINTEXT";

/// The bootstrap member a cluster offering that scheme publishes its brokers under
/// (`GetBootstrapBrokersResponse$BootstrapBrokerString`).
const BOOTSTRAP_PLAINTEXT_MEMBER: &str = "bootstrapBrokerString";

/// How long each function below is given to run one batch. Generous: what is under test is
/// which records reach the handler, never how fast an interpreter gets through one.
const FUNCTION_TIMEOUT: i64 = 60;

/// How long a wait for work a mapping has to do is given before the row is a failure. Well
/// above every settle span the chain crosses — the cluster's, the topic's, the function's, the
/// mapping's and the poller's interval — so a row fails only when the work was not done at all.
const DELIVERY_BUDGET: Duration = Duration::from_secs(60);

/// How long a row watches the sink to show that NOTHING is delivered. It bounds a negative
/// reading, so it is generous rather than tight: a host too slow to deliver inside it cannot
/// make the row fail, only pass for the wrong reason, and the row that follows then asserts the
/// same records DO arrive once polling is resumed.
const QUIET_WATCH: Duration = Duration::from_secs(5);

/// The `eventSource` a record of a Kafka source names itself with, which the handler refuses to
/// process anything else under.
const EVENT_SOURCE: &str = "aws:kafka";

/// The `StartingPosition` values the rows below read from: "the position in a stream from which
/// to start reading" (`CreateEventSourceMappingInput$StartingPosition`).
const TRIM_HORIZON: &str = "TRIM_HORIZON";
/// See [`TRIM_HORIZON`].
const LATEST: &str = "LATEST";

/// What `LastProcessingResult` reports for an attempt that processed the batch
/// (`EventSourceMappingConfiguration$LastProcessingResult`).
const RESULT_OK: &str = "OK";

/// The environment variable the handler reads the cluster it must be told about from.
const CLUSTER_ARN_VARIABLE: &str = "CLUSTER_ARN";

/// What the handler PRINTS before each record's own value, which is how a client watching the
/// function's own log sees which records arrived. ONE spelling of it, bound into the handler's
/// source and into what the assertions look for.
const MARKER: &str = "kafka record: ";

/// The default group a function's invocations are logged to: "By default, Lambda functions send
/// logs to a default log group named `/aws/lambda/<function name>`" (`LoggingConfig$LogGroup`).
const DEFAULT_GROUP_PREFIX: &str = "/aws/lambda/";

/// Lambda, Amazon MSK, Amazon SQS and CloudWatch Logs over ONE shared engine: the mapping's
/// event source is a cluster's topic, the handler forwards what it was handed to a queue and
/// its own lines are delivered to its log group, so all four are reached in process — the shape
/// `core/router/src/lib.rs` mounts them in.
fn emulator() -> MultiServiceRouter {
    emulator_on(&Arc::new(engine::InMemoryEngine::new()))
}

/// The same four services mounted on `engine`, for a suite that needs the engine handle
/// itself — the sibling suite over the consumer GROUP such a mapping reads as
/// ([`crate::kafka_event_source_delivery`]) reads that group out of the store the service
/// which owns it holds, so it brings its own engine and mounts the services on it here rather
/// than carrying a second copy of this bring-up.
pub(crate) fn emulator_on(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = router_on(engine);
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(engine),
    ));
    router.register_service(
        service_com_amazonaws_kafka_kafka::EmulatedService::new(Arc::clone(engine)),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            engine,
        )),
    );
    router
}

/// Percent-encode a URI component: an MSK cluster ARN travels as an `@httpLabel`, so its `:`
/// and `/` are encoded to keep it one path segment. Everything that is not an ASCII
/// alphanumeric is encoded, which is the conservative set (RFC 3986 §2.1), written by the one
/// percent-encoder this workspace declares.
fn pct(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// One call of AMAZON MSK, the service that owns clusters and topics, signed for its own
/// signing name — the one way this suite reaches it, so the topic a mapping consumes is one
/// that service itself reports.
async fn kafka_call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = match body {
        None => Vec::new(),
        Some(body) => serde_json::to_vec(body).expect("the cluster request body must serialize"),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!("http://kafka.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("kafka.{REGION}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/kafka/\
                 aws4_request, SignedHeaders=host, Signature=not-verified"
            ),
        )
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(payload))
        .expect("the cluster request must build");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = response.into_body();
    let parsed = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Create the cluster `name` through Amazon MSK's own `CreateCluster`, answering the ARN that
/// service minted for it.
///
/// `pub(crate)` so the sibling suite over the consumer group a mapping reads as brings a
/// cluster up the one way ([`emulator_on`]).
pub(crate) async fn given_cluster(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, created) = kafka_call(
        router,
        "POST",
        "/v1/clusters",
        Some(&json!({
            "clusterName": name,
            "kafkaVersion": KAFKA_VERSION,
            "numberOfBrokerNodes": 2,
            "brokerNodeGroupInfo": {
                "instanceType": INSTANCE_TYPE,
                "clientSubnets": ["subnet-a", "subnet-b"],
            },
            "encryptionInfo": { "encryptionInTransit": { "clientBroker": PLAINTEXT } },
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateCluster({name:?}) answered {status} {created}"
    );
    created["clusterArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no clusterArn in {created}"))
        .to_string()
}

/// Create the topic `name` with `partitions` partitions on `cluster`, and wait until that
/// service reports it `ACTIVE` — the state in which a client's produce reaches its log.
///
/// `pub(crate)` for the reason [`given_cluster`] is.
pub(crate) async fn given_topic(
    router: &MultiServiceRouter,
    case: &str,
    cluster: &str,
    name: &str,
    partitions: i64,
) {
    let (status, created) = kafka_call(
        router,
        "POST",
        &format!("/v1/clusters/{}/topics", pct(cluster)),
        Some(&json!({
            "topicName": name,
            "partitionCount": partitions,
            "replicationFactor": 1,
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateTopic({name:?}) answered {status} {created}"
    );
    let uri = format!("/v1/clusters/{}/topics/{name}", pct(cluster));
    let deadline = Instant::now() + CLOCK_BUDGET;
    loop {
        let (status, read) = kafka_call(router, "GET", &uri, None).await;
        if status == 200 && read["status"] == json!("ACTIVE") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the topic {name:?} must become ACTIVE, last read {status} {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The first `host:port` pair the cluster publishes its PLAINTEXT brokers at, once it publishes
/// one: `GetBootstrapBrokers` refuses while the cluster is still being provisioned, so this
/// looks again rather than waiting a span out.
async fn published_bootstrap(router: &MultiServiceRouter, case: &str, cluster: &str) -> String {
    let uri = format!("/v1/clusters/{}/bootstrap-brokers", pct(cluster));
    let deadline = Instant::now() + CLOCK_BUDGET;
    loop {
        let (status, read) = kafka_call(router, "GET", &uri, None).await;
        if let Some(published) = read[BOOTSTRAP_PLAINTEXT_MEMBER].as_str() {
            return published
                .split(',')
                .next()
                .unwrap_or(published)
                .trim()
                .to_string();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the cluster must publish {BOOTSTRAP_PLAINTEXT_MEMBER}, last read \
             {status} {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One Apache Kafka PRODUCER on a published bootstrap address: a real socket to the address the
/// control plane announced, framed as the protocol frames a request, so every record below is
/// written to the cluster's log the way a client writes one.
struct Producer {
    stream: TcpStream,
    correlation: i32,
}

impl Producer {
    /// Dial the address the cluster published, after asserting it names this emulator's own
    /// interface: the core binds its listeners on the operator-chosen interface, loopback by
    /// default, so a published bootstrap address is this machine's own — and it is the PORT
    /// that picks the socket out.
    async fn dial(case: &str, authority: &str) -> Self {
        let (host, port) = authority
            .rsplit_once(':')
            .unwrap_or_else(|| panic!("case {case:?}: {authority:?} is not a host:port pair"));
        assert!(
            service_common::data_plane_addressing::names_the_loopback_interface(host),
            "case {case:?}: a published bootstrap address must name this emulator's own \
             interface, got {authority:?}"
        );
        let port: u16 = port
            .parse()
            .unwrap_or_else(|_| panic!("case {case:?}: {port:?} is not a port"));
        // The loopback literal is named on the line that opens the connection, so the address
        // this producer goes to is readable there.
        let stream = TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "case {case:?}: the published address {authority} must accept a connection, \
                     got {error}"
                )
            });
        Self {
            stream,
            correlation: 0,
        }
    }

    /// `Produce` one record batch to one partition, asserting the broker took it, and answer the
    /// offset the first record of the batch took.
    async fn produce(
        &mut self,
        case: &str,
        topic: &str,
        partition: i32,
        records: &[(Option<&str>, Option<&str>)],
    ) -> i64 {
        let stamped = epoch_millis();
        let batch: Vec<Record> = records
            .iter()
            .enumerate()
            .map(|(index, (key, value))| Record {
                transactional: false,
                control: false,
                partition_leader_epoch: 0,
                producer_id: -1,
                producer_epoch: -1,
                timestamp_type: TimestampType::Creation,
                offset: index as i64,
                sequence: index as i32,
                timestamp: stamped,
                key: key.map(|key| Bytes::from(key.as_bytes().to_vec())),
                value: value.map(|value| Bytes::from(value.as_bytes().to_vec())),
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
        let answered: ProduceResponse = self
            .call(
                case,
                ApiKey::Produce,
                9,
                &ProduceRequest::default()
                    .with_acks(1)
                    .with_timeout_ms(5_000)
                    .with_topic_data(vec![TopicProduceData::default()
                        .with_name(TopicName(StrBytes::from_string(topic.to_string())))
                        .with_partition_data(vec![PartitionProduceData::default()
                            .with_index(partition)
                            .with_records(Some(encoded.freeze()))])]),
            )
            .await;
        let reported = answered
            .responses
            .first()
            .and_then(|topic| topic.partition_responses.first())
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: Produce reported no partition"));
        assert_eq!(
            reported.error_code, 0,
            "case {case:?}: the produce to {topic}-{partition} must be accepted, got error {}",
            reported.error_code
        );
        reported.base_offset
    }

    /// Send one request and read its response, framed exactly as the protocol frames them: a
    /// length, the request header for this API and version, and the body.
    async fn call<Request: Encodable, Response: Decodable>(
        &mut self,
        case: &str,
        key: ApiKey,
        version: i16,
        request: &Request,
    ) -> Response {
        self.correlation += 1;
        let mut frame = BytesMut::new();
        RequestHeader::default()
            .with_request_api_key(key as i16)
            .with_request_api_version(version)
            .with_correlation_id(self.correlation)
            .with_client_id(Some(StrBytes::from_static_str("msk-esm-suite")))
            .encode(&mut frame, key.request_header_version(version))
            .unwrap_or_else(|error| panic!("case {case:?}: the header must encode: {error}"));
        request
            .encode(&mut frame, version)
            .unwrap_or_else(|error| panic!("case {case:?}: the request must encode: {error}"));
        let length = u32::try_from(frame.len()).expect("a request frame fits in a u32");
        self.stream
            .write_all(&length.to_be_bytes())
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: the frame must be written: {error}"));
        self.stream
            .write_all(&frame)
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: the frame must be written: {error}"));
        let mut answered = self.read_frame(case).await;
        ResponseHeader::decode(&mut answered, key.response_header_version(version)).unwrap_or_else(
            |error| panic!("case {case:?}: the response header must decode: {error}"),
        );
        Response::decode(&mut answered, version)
            .unwrap_or_else(|error| panic!("case {case:?}: the response must decode: {error}"))
    }

    /// The next response frame the broker writes.
    async fn read_frame(&mut self, case: &str) -> Bytes {
        let mut length = [0u8; 4];
        self.stream
            .read_exact(&mut length)
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: the broker must answer: {error}"));
        let length = u32::from_be_bytes(length) as usize;
        assert!(
            length > 0 && length < 8 * 1024 * 1024,
            "case {case:?}: the broker answered a frame of {length} bytes"
        );
        let mut frame = vec![0u8; length];
        self.stream
            .read_exact(&mut frame)
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: the frame must be read: {error}"));
        Bytes::from(frame)
    }
}

/// The instant a produced record is stamped with, in epoch milliseconds, which is the unit a
/// Kafka record's timestamp carries.
fn epoch_millis() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(0)
}

/// The handler every function below runs: it refuses an event that is not the one Lambda gives a
/// Kafka source, and forwards each record of the batch it was handed to the sink queue its
/// environment names — so a body on that queue is a record that really reached a run.
fn forwarding_handler() -> String {
    format!(
        r#"
import base64
import json
import os

import boto3

EVENT_SOURCE = "{EVENT_SOURCE}"
CLUSTER_ARN = os.environ["{CLUSTER_ARN_VARIABLE}"]


def _text(encoded):
    if encoded is None:
        return None
    return base64.b64decode(encoded).decode("utf-8")


def lambda_handler(event, context):
    if event["eventSource"] != EVENT_SOURCE:
        raise ValueError("eventSource is " + repr(event["eventSource"]))
    if event["eventSourceArn"] != CLUSTER_ARN:
        raise ValueError("eventSourceArn is " + repr(event["eventSourceArn"]))
    if not event.get("bootstrapServers"):
        raise ValueError("the event carries no bootstrapServers")
    client = boto3.client("sqs")
    url = os.environ["{RESULTS_QUEUE_URL}"]
    forwarded = 0
    for partition_key, records in event["records"].items():
        if not records:
            raise ValueError("no record was handed over for " + partition_key)
        for record in records:
            named = record["topic"] + "-" + str(record["partition"])
            if named != partition_key:
                raise ValueError("the records of " + partition_key + " came from " + named)
            value = _text(record.get("value"))
            if value is None:
                value = "tombstone-" + partition_key + "-" + str(record["offset"])
            body = value
            print("{MARKER}" + body)
            client.send_message(QueueUrl=url, MessageBody=json.dumps({{
                "body": body,
                "recordKey": _text(record.get("key")),
                "partitionKey": partition_key,
                "offset": record["offset"],
                "timestampType": record["timestampType"],
            }}))
            forwarded += 1
    return {{"forwarded": forwarded}}
"#
    )
}

/// Stand up the chain one row drives: a cluster, a topic on it, a sink queue, a function
/// forwarding to that queue, and a producer dialled at the address the cluster published.
struct Chain {
    router: MultiServiceRouter,
    cluster: String,
    topic: String,
    sink: String,
    producer: Producer,
}

impl Chain {
    /// Everything but the mapping: the mapping is what each row configures for itself.
    async fn given(case: &str, slug: &str, partitions: i64) -> Self {
        let router = emulator();
        let cluster = given_cluster(&router, case, &format!("{slug}-cluster")).await;
        let topic = format!("{slug}-topic");
        given_topic(&router, case, &cluster, &topic, partitions).await;
        let address = published_bootstrap(&router, case, &cluster).await;
        let producer = Producer::dial(case, &address).await;
        let (sink, _arn) = create_queue(&router, case, &format!("{slug}-sink"), &json!({})).await;
        create_running_function(
            &router,
            case,
            &format!("{slug}-fn"),
            &forwarding_handler(),
            FUNCTION_TIMEOUT,
            &json!({
                RESULTS_QUEUE_URL: sink.clone(),
                CLUSTER_ARN_VARIABLE: cluster.clone(),
            }),
        )
        .await;
        Self {
            router,
            cluster,
            topic,
            sink,
            producer,
        }
    }

    /// The function every mapping of this chain invokes.
    fn function(&self, slug: &str) -> String {
        format!("{slug}-fn")
    }
}

/// Every record the handler has forwarded so far, added to `held` and keyed by the body it
/// reports: the one reader of a delivery in this suite.
async fn collect(
    router: &MultiServiceRouter,
    sink: &str,
    held: &mut BTreeMap<String, Value>,
    wanted: usize,
    budget: Duration,
) {
    let arrived = forwarded_records(router, sink, wanted, budget).await;
    held.extend(arrived);
}

/// The bodies `held` holds, in the order a reader keyed by body reports them, which is what a
/// row asserts against: what the sink holds is a SET of records — the queue hands them over as
/// they arrive and each is read once — so the ORDER records were delivered in is asserted
/// through the offsets they report rather than through this reading.
fn bodies(held: &BTreeMap<String, Value>) -> Vec<String> {
    held.keys().cloned().collect()
}

/// `wanted` as [`bodies`] reports the same set, so an expectation is held against it whatever
/// order the records arrived in.
fn expected_bodies(wanted: &[&str]) -> Vec<String> {
    let mut expected: Vec<String> = wanted.iter().map(|body| (*body).to_string()).collect();
    expected.sort();
    expected
}

/// `UpdateEventSourceMapping` on one mapping with `change`, asserting it was accepted.
async fn update_mapping(router: &MultiServiceRouter, case: &str, uuid: &str, change: &Value) {
    let answered = call(
        router,
        "PUT",
        &format!("{MAPPINGS_URI}/{uuid}"),
        change,
    )
    .await;
    assert_eq!(
        answered.status,
        202,
        "case {case:?}: expected UpdateEventSourceMapping to answer 202, got {} {}",
        answered.status,
        answered.text()
    );
}

/// Wait until the mapping reports `RESULT_OK` as its last processing attempt, which is what a
/// pass that handed a batch over records.
async fn await_ok_attempt(router: &MultiServiceRouter, case: &str, uuid: &str) {
    let deadline = Instant::now() + DELIVERY_BUDGET;
    loop {
        let read = get_mapping(router, uuid).await;
        if read.string("LastProcessingResult").as_deref() == Some(RESULT_OK) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the mapping must report {RESULT_OK:?} as its LastProcessingResult, \
             got {}",
            read.text()
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// RULE: exactly the records the mapping's `FilterCriteria` admit are handed to the function,
/// the records they reject are handed to no invocation at all, and the records behind a rejected
/// one are still delivered; once the criteria are DELETED — "to delete the filter criteria,
/// specify an empty FilterCriteria object" — every record produced afterwards is delivered
/// whatever it carries.
///
/// The records of the row differ along what the rule generalises over: a value spelling a
/// document the pattern matches, one spelling a document it does not, one that is plain text and
/// no document at all, and a record carrying a key beside one that does not.
#[tokio::test]
async fn only_the_records_the_criteria_admit_are_delivered_and_clearing_them_releases_the_rest() {
    let case = "filter criteria over an MSK topic";
    let slug = "msk-filter";
    let mut chain = Chain::given(case, slug, 1).await;
    let function = chain.function(slug);
    let uuid = given_enabled_mapping(
        &chain.router,
        case,
        &json!({
            "FunctionName": function,
            "EventSourceArn": chain.cluster,
            "Topics": [chain.topic],
            "StartingPosition": TRIM_HORIZON,
            "BatchSize": 10,
            "Enabled": true,
            "FilterCriteria": {
                "Filters": [{ "Pattern": r#"{"value": {"department": ["finance"]}}"# }],
            },
        }),
    )
    .await;

    // The ONE client action: the records are produced where a Kafka client produces them.
    let admitted_first = r#"{"department":"finance","id":"a"}"#;
    let rejected = r#"{"department":"retail","id":"b"}"#;
    let admitted_second = r#"{"department":"finance","id":"c"}"#;
    let plain = "not a document at all";
    chain
        .producer
        .produce(
            case,
            &chain.topic,
            0,
            &[
                (Some("key-a"), Some(admitted_first)),
                (None, Some(rejected)),
                (Some("key-c"), Some(admitted_second)),
                (None, Some(plain)),
            ],
        )
        .await;

    let mut held: BTreeMap<String, Value> = BTreeMap::new();
    collect(&chain.router, &chain.sink, &mut held, 2, DELIVERY_BUDGET).await;
    assert_eq!(
        bodies(&held),
        expected_bodies(&[admitted_first, admitted_second]),
        "case {case:?}: expected only the records the pattern matches, got {:?}; the mapping \
         reports {}",
        bodies(&held),
        get_mapping(&chain.router, &uuid).await.text()
    );
    // The record behind the rejected one arrived, so the position moved past a record no
    // invocation was handed.
    let second = &held[admitted_second];
    assert_eq!(
        second["partitionKey"],
        json!(format!("{}-0", chain.topic)),
        "case {case:?}: expected the record keyed by its topic and partition, got {second}"
    );
    assert_eq!(
        second["recordKey"],
        json!("key-c"),
        "case {case:?}: expected the key the producer wrote, got {second}"
    );
    assert_eq!(
        second["timestampType"],
        json!("CREATE_TIME"),
        "case {case:?}: expected the producer's own timestamp type, got {second}"
    );
    await_ok_attempt(&chain.router, case, &uuid).await;

    // The criteria are DELETED, and what is produced afterwards is delivered whatever it
    // carries — including a value the old pattern would have rejected.
    update_mapping(
        &chain.router,
        case,
        &uuid,
        &json!({ "FilterCriteria": {} }),
    )
    .await;
    await_mapping_state(&chain.router, case, &uuid, STATE_ENABLED).await;
    let released = r#"{"department":"retail","id":"d"}"#;
    chain
        .producer
        .produce(case, &chain.topic, 0, &[(None, Some(released))])
        .await;
    collect(&chain.router, &chain.sink, &mut held, 1, DELIVERY_BUDGET).await;
    assert!(
        held.contains_key(released),
        "case {case:?}: a record produced after the criteria were deleted must be delivered, \
         got {:?}; the mapping reports {}",
        bodies(&held),
        get_mapping(&chain.router, &uuid).await.text()
    );
    // And the records the criteria rejected while they were in force reached no invocation at
    // all, which the whole run has now had the patience to show.
    for refused in [rejected, plain] {
        assert!(
            !held.contains_key(refused),
            "case {case:?}: the record {refused:?} matched no pattern and must reach no \
             invocation, got {:?}",
            bodies(&held)
        );
    }
    // The handler's OWN lines are where a client watching the function sees which records
    // arrived: one line per record it was handed, in the function's own log group.
    let group = format!("{DEFAULT_GROUP_PREFIX}{function}");
    let wanted: Vec<String> = [admitted_first, admitted_second, released]
        .iter()
        .map(|body| format!("{MARKER}{body}"))
        .collect();
    let lines = logged(&chain.router, case, &group, &wanted, DELIVERY_BUDGET).await;
    let missing: Vec<&String> = wanted
        .iter()
        .filter(|line| !lines.iter().any(|written| written.contains(line.as_str())))
        .collect();
    assert!(
        missing.is_empty(),
        "case {case:?}: the handler's lines {missing:?} must be delivered to {group:?}, got \
         {lines:?}"
    );
    for refused in [rejected, plain] {
        let line = format!("{MARKER}{refused}");
        assert!(
            !lines.iter().any(|written| written.contains(&line)),
            "case {case:?}: no handler line may report {refused:?}, got {lines:?}"
        );
    }
    // The same lines a client that SEARCHES the group reads, which is the other way a client
    // watching a function reads what its handler wrote.
    let (status, filtered) = crate::invocation_log_delivery::logs_answer(
        &chain.router,
        case,
        "FilterLogEvents",
        &json!({ "logGroupName": group }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: FilterLogEvents answered {status} {filtered}"
    );
    let searched: Vec<String> = filtered["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|event| event["message"].as_str().map(str::to_string))
        .collect();
    let missing: Vec<&String> = wanted
        .iter()
        .filter(|line| !searched.iter().any(|written| written.contains(line.as_str())))
        .collect();
    assert!(
        missing.is_empty(),
        "case {case:?}: a search of {group:?} must find the handler's lines {missing:?}, got \
         {searched:?}"
    );
}

/// Watch the function's log group until every line of `wanted` has been delivered, answering
/// what was last read.
///
/// The whole chain is waited for TOGETHER — the poller taking the batch, the invocation running
/// and its log reaching the group — because a reader that stopped at the first of them would be
/// reading an instant of its own choosing in the middle of it.
async fn logged(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    wanted: &[String],
    budget: Duration,
) -> Vec<String> {
    let deadline = Instant::now() + budget;
    loop {
        let lines = crate::sqs_event_source_log_delivery::logged_lines(router, case, group).await;
        let settled = wanted
            .iter()
            .all(|line| lines.iter().any(|written| written.contains(line)));
        if settled || Instant::now() >= deadline {
            return lines;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// RULE: "when false, Lambda pauses polling and invocation" — a mapping whose polling is paused
/// delivers nothing — and the position is the MAPPING's, so pausing does not move it: once
/// polling is resumed the records produced while it was paused are delivered, from the offset
/// the mapping kept.
#[tokio::test]
async fn a_paused_mapping_delivers_nothing_and_once_resumed_delivers_the_records_it_missed() {
    let case = "polling paused and resumed over an MSK topic";
    let slug = "msk-paused";
    let mut chain = Chain::given(case, slug, 1).await;
    let function = chain.function(slug);
    let uuid = given_enabled_mapping(
        &chain.router,
        case,
        &json!({
            "FunctionName": function,
            "EventSourceArn": chain.cluster,
            "Topics": [chain.topic],
            "StartingPosition": TRIM_HORIZON,
            "BatchSize": 10,
            "Enabled": true,
        }),
    )
    .await;

    let before = "produced-while-polling";
    chain
        .producer
        .produce(case, &chain.topic, 0, &[(None, Some(before))])
        .await;
    let mut held: BTreeMap<String, Value> = BTreeMap::new();
    collect(&chain.router, &chain.sink, &mut held, 1, DELIVERY_BUDGET).await;
    assert_eq!(
        bodies(&held),
        expected_bodies(&[before]),
        "case {case:?}: expected the record produced while polling to be delivered, got {:?}; \
         the mapping reports {}",
        bodies(&held),
        get_mapping(&chain.router, &uuid).await.text()
    );

    // Polling is PAUSED, and the records produced while it is paused are delivered to nothing.
    update_mapping(&chain.router, case, &uuid, &json!({ "Enabled": false })).await;
    await_mapping_state(&chain.router, case, &uuid, STATE_DISABLED).await;
    let missed = ["produced-while-paused-1", "produced-while-paused-2"];
    chain
        .producer
        .produce(
            case,
            &chain.topic,
            0,
            &[(None, Some(missed[0])), (None, Some(missed[1]))],
        )
        .await;
    let mut quiet: BTreeMap<String, Value> = BTreeMap::new();
    collect(&chain.router, &chain.sink, &mut quiet, 1, QUIET_WATCH).await;
    assert!(
        quiet.is_empty(),
        "case {case:?}: a mapping with polling paused must deliver nothing, got {:?}",
        bodies(&quiet)
    );
    let paused = get_mapping(&chain.router, &uuid).await;
    assert!(
        paused.string("LastProcessingResult").is_none(),
        "case {case:?}: a mapping nothing is polling reports no processing attempt, got {}",
        paused.text()
    );

    // Polling is RESUMED, and the backlog it built up while paused is delivered from the offset
    // the mapping kept — in the order the log holds it.
    update_mapping(&chain.router, case, &uuid, &json!({ "Enabled": true })).await;
    await_mapping_state(&chain.router, case, &uuid, STATE_ENABLED).await;
    collect(&chain.router, &chain.sink, &mut held, 3, DELIVERY_BUDGET).await;
    assert_eq!(
        bodies(&held),
        expected_bodies(&[before, missed[0], missed[1]]),
        "case {case:?}: expected the records produced while paused to be delivered once \
         polling resumed, got {:?}; the mapping reports {}",
        bodies(&held),
        get_mapping(&chain.router, &uuid).await.text()
    );
    let offsets: Vec<i64> = [missed[0], missed[1]]
        .iter()
        .filter_map(|body| held.get(*body).and_then(|record| record["offset"].as_i64()))
        .collect();
    assert_eq!(
        offsets,
        vec![1, 2],
        "case {case:?}: expected the missed records to be delivered from the offsets they were \
         produced at, got {offsets:?}"
    );
}

/// One row of the table below: how the topic is shaped, what is produced to it, and how the
/// mapping reads it.
struct Row {
    case: &'static str,
    slug: &'static str,
    /// How many partitions the topic has, so the records of several partitions are read.
    partitions: i64,
    /// The `BatchSize` the mapping reads with.
    batch_size: i64,
    /// The `StartingPosition` the mapping is created with.
    starting_position: &'static str,
}

/// The records one row produces to `partition`: a keyed record whose value spells a document,
/// an unkeyed one whose value is plain text, and a tombstone that carries no value at all —
/// each naming the partition it was produced to, so a delivery is attributable to the log it
/// came out of.
fn produced(partition: i32) -> Vec<(Option<String>, Option<String>)> {
    vec![
        (
            Some(format!("key-{partition}-0")),
            Some(format!(r#"{{"id":0,"partition":{partition}}}"#)),
        ),
        (None, Some(format!("plain text on partition {partition}"))),
        (Some(format!("key-{partition}-2")), None),
    ]
}

/// The body the handler forwards for the `index`th record produced to `partition` of `topic`:
/// the record's own value, and for a tombstone the partition and offset it sat at — which is
/// what makes the record of one partition's tombstone tell itself from another's.
fn expected_body(topic: &str, partition: i32, index: usize) -> String {
    match produced(partition)
        .get(index)
        .and_then(|(_key, value)| value.clone())
    {
        Some(value) => value,
        None => format!("tombstone-{topic}-{partition}-{index}"),
    }
}

/// RULE (the same one, over the shapes it generalises over): every record produced to a
/// partition of a topic the mapping names is handed to the function, in the order the log holds
/// it, whatever its key or value is — read in batches of at most `BatchSize`, from the position
/// the mapping's `StartingPosition` fixes.
///
/// The rows differ along what the rule generalises over: one partition and several, a batch
/// ceiling above what is waiting and one record per batch, both positions a Kafka source is
/// read from, keyed and unkeyed records, values that spell a document and values that are plain
/// text, and a tombstone that carries no value at all.
async fn assert_every_record_is_delivered(row: &Row) {
    let case = row.case;
    let mut chain = Chain::given(case, row.slug, row.partitions).await;
    let function = chain.function(row.slug);
    let topic = chain.topic.clone();
    // A mapping reading from TRIM_HORIZON reads what the log already holds, so its records are
    // produced BEFORE it is created; one reading from LATEST reads what arrives after its own
    // position, so its records are produced once it is polling.
    let produce_first = row.starting_position == TRIM_HORIZON;
    if produce_first {
        produce_every_partition(&mut chain, case, row).await;
    }
    let uuid = given_enabled_mapping(
        &chain.router,
        case,
        &json!({
            "FunctionName": function,
            "EventSourceArn": chain.cluster,
            "Topics": [topic.clone()],
            "StartingPosition": row.starting_position,
            "BatchSize": row.batch_size,
            "Enabled": true,
        }),
    )
    .await;
    if !produce_first {
        produce_every_partition(&mut chain, case, row).await;
    }

    let per_partition = produced(0).len();
    let wanted = per_partition * usize::try_from(row.partitions).unwrap_or(1);
    let mut held: BTreeMap<String, Value> = BTreeMap::new();
    collect(
        &chain.router,
        &chain.sink,
        &mut held,
        wanted,
        DELIVERY_BUDGET,
    )
    .await;
    assert_eq!(
        held.len(),
        wanted,
        "case {case:?}: expected {wanted} record(s) to be delivered, got {:?}; the mapping \
         reports {}",
        bodies(&held),
        get_mapping(&chain.router, &uuid).await.text()
    );
    // Every record is reported by the partition it came from, with the key its producer wrote
    // and the offset it took in that partition's log.
    for partition in 0..row.partitions {
        let partition = i32::try_from(partition).unwrap_or_default();
        for (index, (key, _value)) in produced(partition).into_iter().enumerate() {
            let body = expected_body(&topic, partition, index);
            let offset = i64::try_from(index).unwrap_or_default();
            let record = held.get(&body).unwrap_or_else(|| {
                panic!(
                    "case {case:?}: the record {body:?} of {topic}-{partition} must be \
                     delivered, got {:?}",
                    bodies(&held)
                )
            });
            assert_eq!(
                record["recordKey"],
                key.map_or(Value::Null, |key| json!(key)),
                "case {case:?}: expected the key the producer wrote for {body:?}, got {record}"
            );
            assert_eq!(
                record["partitionKey"],
                json!(format!("{topic}-{partition}")),
                "case {case:?}: expected {body:?} keyed by the partition it came from, got \
                 {record}"
            );
            assert_eq!(
                record["offset"],
                json!(offset),
                "case {case:?}: expected {body:?} at the offset it was produced at, got {record}"
            );
        }
    }
    await_ok_attempt(&chain.router, case, &uuid).await;
}

/// Produce the row's records to every partition of its topic, over the published address.
async fn produce_every_partition(chain: &mut Chain, case: &str, row: &Row) {
    let topic = chain.topic.clone();
    for partition in 0..row.partitions {
        let partition = i32::try_from(partition).unwrap_or_default();
        let records = produced(partition);
        let batch: Vec<(Option<&str>, Option<&str>)> = records
            .iter()
            .map(|(key, value)| (key.as_deref(), value.as_deref()))
            .collect();
        chain
            .producer
            .produce(case, &topic, partition, &batch)
            .await;
    }
}

/// See [`assert_every_record_is_delivered`]: one partition, a batch ceiling above what is
/// waiting, read from the horizon.
#[tokio::test]
async fn one_partition_read_from_the_horizon_delivers_every_record() {
    assert_every_record_is_delivered(&Row {
        case: "one partition, a batch above what is waiting, from the horizon",
        slug: "msk-horizon",
        partitions: 1,
        batch_size: 10,
        starting_position: TRIM_HORIZON,
    })
    .await;
}

/// See [`assert_every_record_is_delivered`]: two partitions, one record per batch, read from the
/// latest position.
#[tokio::test]
async fn two_partitions_read_one_record_per_batch_from_the_latest_position() {
    assert_every_record_is_delivered(&Row {
        case: "two partitions, one record per batch, from the latest position",
        slug: "msk-latest",
        partitions: 2,
        batch_size: 1,
        starting_position: LATEST,
    })
    .await;
}
