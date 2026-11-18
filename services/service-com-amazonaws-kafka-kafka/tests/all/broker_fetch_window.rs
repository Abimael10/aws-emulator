// SPDX-License-Identifier: CC0-1.0

//! What a `Fetch` over the published bootstrap address answers, and WHEN.
//!
//! RULE under test: a fetch answers the records of the partition from the offset it asked
//! for, each carrying the offset it was written at — so a consumer that resumes from an
//! offset it committed reads on from there rather than reading the log again. And a fetch
//! that finds less than the `MinBytes` it asked for is HELD until that much is there or
//! its `MaxWaitMs` has run out, which is what those two members of the request mean: a
//! consumer polling an idle partition waits on the broker instead of being answered an
//! empty response at once and asking again immediately.
//!
//! The rows span what the rule generalises over: the OFFSET a fetch starts at (the first,
//! one inside the log, the last, and the end of the log), and whether the partition holds
//! what the fetch asked for when it arrives.
//!
//! No row asserts on elapsed time: the long-poll row asserts the OUTCOME — a fetch issued
//! before the record exists answers that record — which is what a consumer depends on.

use ::kafka_protocol::messages::{
    fetch_request::{FetchPartition, FetchTopic},
    ApiKey, FetchRequest, FetchResponse, TopicName,
};
use ::kafka_protocol::protocol::StrBytes;
use ::kafka_protocol::records::RecordBatchDecoder;
use serde_json::json;

use crate::broker_data_plane::Client;
use crate::kafka_harness::{pct, ClusterRequest, Emulator};

/// How long a held fetch below may wait for the record another connection is about to
/// produce: generous, so the row says nothing about how fast the host runs. The answer
/// comes as soon as the record lands.
const LONG_POLL_MS: i32 = 30_000;

/// RULE: a fetch from an offset inside the log answers the records from that offset on,
/// each carrying the offset it was written at.
#[tokio::test]
async fn a_fetch_answers_the_records_from_the_offset_it_asked_for() {
    let case = "a resumed fetch";
    let emulator = Emulator::new();
    let cluster = given_plaintext_cluster(&emulator, "fetch-window").await;
    given_topic(&emulator, &cluster, "orders").await;
    let authority = plaintext_address(&emulator, &cluster, case).await;
    let mut client = Client::plaintext(case, &authority).await;
    client.api_versions(case).await;

    let produced: Vec<(i64, Option<&str>, &str)> = (0..5)
        .map(|index| (-1i64, None, VALUES[index as usize]))
        .collect();
    let base = client.produce(case, "orders", 0, &produced).await;
    assert_eq!(
        base, 0,
        "case {case:?}: the first batch of an empty partition starts at offset 0"
    );

    // (the offset the fetch starts at, the offsets it must answer)
    let rows: &[(i64, &[i64])] = &[
        (0, &[0, 1, 2, 3, 4]),
        (2, &[2, 3, 4]),
        (4, &[4]),
        (5, &[]),
    ];
    for (offset, expected) in rows {
        let read = fetch(&mut client, case, "orders", *offset, 1, 0).await;
        let offsets: Vec<i64> = read.iter().map(|(offset, _)| *offset).collect();
        assert_eq!(
            offsets, *expected,
            "case {case:?}: a fetch from offset {offset} must answer the records at \
             {expected:?}, got {offsets:?}"
        );
        for (at, value) in &read {
            let index = usize::try_from(*at).unwrap_or_default();
            assert_eq!(
                value.as_str(),
                VALUES[index],
                "case {case:?}: the record at offset {at} must carry what was produced \
                 there, got {value:?}"
            );
        }
    }
}

/// RULE: a fetch that finds less than its `MinBytes` waits for records rather than
/// answering empty — so a fetch in flight when a record is produced answers that record,
/// while a fetch that asks to wait for nothing is answered at once.
#[tokio::test]
async fn a_fetch_waits_for_the_records_it_asked_for() {
    let case = "a held fetch";
    let emulator = Emulator::new();
    let cluster = given_plaintext_cluster(&emulator, "fetch-wait").await;
    given_topic(&emulator, &cluster, "orders").await;
    let authority = plaintext_address(&emulator, &cluster, case).await;

    // A fetch that asks to wait for nothing is answered out of what the partition holds
    // now, which is nothing.
    let mut reader = Client::plaintext(case, &authority).await;
    reader.api_versions(case).await;
    let at_once = fetch(&mut reader, case, "orders", 0, 0, 0).await;
    assert!(
        at_once.is_empty(),
        "case {case:?}: a fetch asking to wait for nothing must answer the empty log, \
         got {at_once:?}"
    );

    // A fetch that asks for a byte, on a partition that holds none, is answered the
    // record another connection produces while it is in flight.
    let mut writer = Client::plaintext(case, &authority).await;
    writer.api_versions(case).await;
    let held = fetch(&mut reader, case, "orders", 0, 1, LONG_POLL_MS);
    let producing = async {
        writer
            .produce(case, "orders", 0, &[(-1, None, VALUES[0])])
            .await
    };
    let (read, base) = ::tokio::join!(held, producing);
    assert_eq!(
        base, 0,
        "case {case:?}: the produced record must take the partition's first offset"
    );
    assert_eq!(
        read.iter()
            .map(|(offset, value)| (*offset, value.clone()))
            .collect::<Vec<_>>(),
        vec![(0, VALUES[0].to_string())],
        "case {case:?}: the held fetch must answer the record that landed while it \
         waited, got {read:?}"
    );
}

/// The record values this suite produces, one per offset — distinct, so a row that reads
/// from an offset can tell which record it got.
const VALUES: [&str; 5] = ["alpha", "beta", "gamma", "delta", "epsilon"];

/// `Fetch` one partition from `offset`, asking for `min_bytes` and waiting at most
/// `max_wait_ms` for them; answers the records as `(offset, value)`.
async fn fetch(
    client: &mut Client,
    case: &str,
    topic: &str,
    offset: i64,
    min_bytes: i32,
    max_wait_ms: i32,
) -> Vec<(i64, String)> {
    let answered: FetchResponse = client
        .call(
            case,
            ApiKey::Fetch,
            11,
            &FetchRequest::default()
                .with_max_wait_ms(max_wait_ms)
                .with_min_bytes(min_bytes)
                .with_max_bytes(1_048_576)
                .with_topics(vec![FetchTopic::default()
                    .with_topic(TopicName(StrBytes::from_string(topic.to_string())))
                    .with_partitions(vec![FetchPartition::default()
                        .with_partition(0)
                        .with_fetch_offset(offset)
                        .with_partition_max_bytes(1_048_576)])]),
        )
        .await;
    let partition = answered
        .responses
        .first()
        .and_then(|topic| topic.partitions.first())
        .unwrap_or_else(|| panic!("case {case:?}: Fetch reported no partition"))
        .clone();
    assert_eq!(
        partition.error_code, 0,
        "case {case:?}: the fetch must be answered, got error {}",
        partition.error_code
    );
    let Some(mut records) = partition.records.clone().filter(|records| !records.is_empty()) else {
        return Vec::new();
    };
    RecordBatchDecoder::decode(&mut records)
        .unwrap_or_else(|error| panic!("case {case:?}: the batch must decode: {error}"))
        .records
        .iter()
        .map(|record| {
            (
                record.offset,
                record
                    .value
                    .as_ref()
                    .map(|value| String::from_utf8_lossy(value).to_string())
                    .unwrap_or_default(),
            )
        })
        .collect()
}

/// A cluster whose brokers are reached in the clear.
async fn given_plaintext_cluster(emulator: &Emulator, name: &str) -> String {
    emulator
        .create_ok(&ClusterRequest {
            client_broker: Some("PLAINTEXT"),
            ..ClusterRequest::named(name)
        })
        .await
}

/// Create a one-partition topic on `cluster` through the control plane.
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
        reply.status, reply.body
    );
}

/// The address a cluster publishes for its plaintext path, once it publishes one.
async fn plaintext_address(emulator: &Emulator, cluster: &str, case: &str) -> String {
    emulator
        .published_bootstrap(cluster, "bootstrapBrokerString", case)
        .await
}
