// SPDX-License-Identifier: CC0-1.0

//! How this service refuses a `ShardId` the stream it addresses does not hold — through
//! EVERY operation addressed by one, end to end over the wire.
//!
//! THE RULE: a request naming a shard that is no shard of the stream it addresses is refused
//! with the modelled `ResourceNotFoundException` (HTTP 400) and the sentence this service
//! words that condition with for the operation addressed: `Shard <shardId> in stream
//! <streamName> under account <accountId> does not exist` for the reshards and the
//! subscription, and `Could not find shard <shardId> in stream <streamName> under account
//! <accountId>.` where `GetShardIterator` searched the resolved stream's shard map. Either
//! sentence is fixed text whose variable parts come from the request and from the emulator's
//! own state — the shard id the request named, the name of the stream the request resolved,
//! and the account the caller speaks for. The request asked for a SHARD, so that is what the
//! refusal is worded at, whether the shard is missing because the stream holds no such entry
//! or because this account and region hold no such stream at all.
//!
//! The rows span what the rule generalises over: which OPERATION is addressed by the shard
//! id (`GetShardIterator`, `SubscribeToShard`, `SplitShard`, `MergeShards`), which SHARD id
//! is asked for (one of a shape this service mints but the map does not carry, one of another
//! stream's map, and one of no shape it mints at all), and whether the stream the request
//! addresses is one this account holds.
//!
//! The sentence is read from the crate's own statement of it
//! (`state::shard_not_found_message`) as well as written out here, so neither can drift from
//! the other. Every wait POLLS for the condition it waits on.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_kinesis_kinesis_20131202::ops::get_shard_iterator::could_not_find_shard_message;
use service_com_amazonaws_kinesis_kinesis_20131202::state::{
    shard_not_found_message, SHARD_ITERATOR_TYPE_TRIM_HORIZON, STREAM_ACTIVE,
};

use crate::stream_tags::{error_code, error_message, stream_arn, Emulator, REGION};

/// How long a poll loop keeps looking before it gives up. Generous: the transition is owed by
/// the emulator's clock, and nothing here measures how long it took.
const DEADLINE: Duration = Duration::from_secs(120);

/// How long a poll loop waits between two looks.
const BETWEEN_LOOKS: Duration = Duration::from_millis(25);

/// The shard id of the shard at `index` of a stream's map, as this service mints them.
fn shard_id(index: usize) -> String {
    format!("shardId-{index:012}")
}

/// Create `name` with `shards` shards and look until the `CREATING` -> `ACTIVE` transition
/// the clock owes it has happened. Answers the stream's ARN.
async fn active_stream(emulator: &Emulator, name: &str, shards: i64) -> String {
    emulator
        .ok(
            name,
            "CreateStream",
            &json!({ "StreamName": name, "ShardCount": shards }),
        )
        .await;
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, body) = emulator
            .call("DescribeStreamSummary", &json!({ "StreamName": name }))
            .await;
        if status == 200
            && body["StreamDescriptionSummary"]["StreamStatus"] == json!(STREAM_ACTIVE)
        {
            return stream_arn(REGION, name);
        }
        assert!(
            Instant::now() < deadline,
            "{name:?} never reached {STREAM_ACTIVE}: last answer {status} with {body}"
        );
        tokio::time::sleep(BETWEEN_LOOKS).await;
    }
}

/// Register a consumer on `stream_arn` and look until it is `ACTIVE`, the state a
/// subscription needs it in. Answers its `ConsumerARN`.
async fn active_consumer(emulator: &Emulator, stream_arn: &str, name: &str) -> String {
    let registered = emulator
        .ok(
            name,
            "RegisterStreamConsumer",
            &json!({ "StreamARN": stream_arn, "ConsumerName": name }),
        )
        .await;
    let consumer_arn = registered["Consumer"]["ConsumerARN"]
        .as_str()
        .unwrap_or_else(|| panic!("RegisterStreamConsumer answers a ConsumerARN, got {registered}"))
        .to_string();
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, body) = emulator
            .call(
                "DescribeStreamConsumer",
                &json!({ "ConsumerARN": consumer_arn }),
            )
            .await;
        if status == 200 && body["ConsumerDescription"]["ConsumerStatus"] == json!("ACTIVE") {
            return consumer_arn;
        }
        assert!(
            Instant::now() < deadline,
            "the consumer {name:?} never became ACTIVE: last answer {status} with {body}"
        );
        tokio::time::sleep(BETWEEN_LOOKS).await;
    }
}

/// The request each operation addresses `shard` of `stream` with. Every member beside the
/// shard id is one the operation needs in order to REACH the shard lookup.
fn request(operation: &str, stream: &str, consumer_arn: &str, shard: &str) -> Value {
    match operation {
        "GetShardIterator" => json!({
            "StreamName": stream,
            "ShardId": shard,
            "ShardIteratorType": SHARD_ITERATOR_TYPE_TRIM_HORIZON,
        }),
        "SubscribeToShard" => json!({
            "ConsumerARN": consumer_arn,
            "ShardId": shard,
            "StartingPosition": { "Type": "TRIM_HORIZON" },
        }),
        "SplitShard" => json!({
            "StreamName": stream,
            "ShardToSplit": shard,
            // A hash key inside the 128-bit key space, so the split is refused for the
            // shard it names rather than for the key.
            "NewStartingHashKey": "1024",
        }),
        _ => json!({
            "StreamName": stream,
            "ShardToMerge": shard,
            "AdjacentShardToMerge": shard_id(1),
        }),
    }
}

/// Assert `operation` refused `shard` of `stream` with the modelled not-found and the
/// sentence `expected`.
fn assert_refused_with(case: &str, answer: &(u16, Value), expected: &str) {
    let (status, body) = answer;
    assert_eq!(
        *status, 400,
        "case {case}: expected the modelled client refusal, got {status} with {body}"
    );
    assert_eq!(
        error_code(body),
        "ResourceNotFoundException",
        "case {case}: expected the modelled not-found, got {body}"
    );
    assert_eq!(
        error_message(body),
        expected,
        "case {case}: expected the message {expected:?}, got {:?}",
        error_message(body)
    );
}

/// Assert `operation` refused `shard` of `stream` with the modelled not-found and the one
/// sentence the rule requires for the class.
fn assert_refused(case: &str, answer: &(u16, Value), stream: &str, shard: &str) {
    let expected = format!(
        "Shard {shard} in stream {stream} under account {} does not exist",
        engine::EMULATOR_ACCOUNT_ID
    );
    assert_refused_with(case, answer, &expected);
    assert_eq!(
        error_message(&answer.1),
        shard_not_found_message(shard, stream),
        "case {case}: the crate's own sentence is the one the wire carries"
    );
}

/// Assert `GetShardIterator` refused `shard` of the stream it RESOLVED with its own
/// sentence: the search of that stream's shard map that came up empty.
fn assert_shard_search_failed(case: &str, answer: &(u16, Value), stream: &str, shard: &str) {
    let expected = format!(
        "Could not find shard {shard} in stream {stream} under account {}.",
        engine::EMULATOR_ACCOUNT_ID
    );
    assert_refused_with(case, answer, &expected);
    assert_eq!(
        error_message(&answer.1),
        could_not_find_shard_message(shard, stream),
        "case {case}: the crate's own sentence is the one the wire carries"
    );
}

/// RULE: every operation addressed by a `ShardId` refuses a shard the stream it addresses
/// does not hold with the same sentence, whichever shard id it is.
#[tokio::test]
async fn every_operation_addressed_by_a_shard_id_states_one_sentence() {
    let emulator = Emulator::new();
    // The stream the requests address, and a SECOND stream whose map carries a shard the
    // first one's does not — the recorded case's shape.
    let stream = "absent-shard-own";
    let other = "absent-shard-other";
    let arn = active_stream(&emulator, stream, 2).await;
    active_stream(&emulator, other, 4).await;
    let consumer_arn = active_consumer(&emulator, &arn, "absent-shard-consumer").await;

    // Each row: the shard id asked for.
    let shards = [
        (
            "a shard id of the shape this service mints that the map does not carry",
            shard_id(99),
        ),
        (
            "a shard only the other stream's map carries",
            shard_id(3),
        ),
        ("a shard id of no shape this service mints", "shard-9".to_string()),
        ("the first shard id of a wider map", shard_id(7)),
    ];
    for (case, shard) in &shards {
        for operation in ["SubscribeToShard", "SplitShard", "MergeShards"] {
            let answer = emulator
                .call(operation, &request(operation, stream, &consumer_arn, shard))
                .await;
            assert_refused(
                &format!("{case} through {operation}"),
                &answer,
                stream,
                shard,
            );
        }

        // `GetShardIterator` words the SAME condition as the search of the resolved
        // stream's shard map that came up empty, which is the sentence the recorded
        // contract carries for it.
        let answer = emulator
            .call(
                "GetShardIterator",
                &request("GetShardIterator", stream, &consumer_arn, shard),
            )
            .await;
        assert_shard_search_failed(
            &format!("{case} through GetShardIterator"),
            &answer,
            stream,
            shard,
        );
    }

    // And the shard the stream DOES hold is still served through the same operation, so the
    // refusal is about the shard asked for rather than about the operation.
    let served = emulator
        .call(
            "GetShardIterator",
            &request(
                "GetShardIterator",
                stream,
                &consumer_arn,
                &shard_id(0),
            ),
        )
        .await;
    assert_eq!(
        served.0, 200,
        "the shard the stream holds is still served, got {} with {:?}",
        served.0, served.1
    );
}

/// RULE, the other half: a shard of a stream this account and region do not hold AT ALL is
/// the same sentence — the request asked for a shard, so the refusal is worded at the shard.
#[tokio::test]
async fn a_shard_of_a_stream_nothing_holds_states_the_same_sentence() {
    let emulator = Emulator::new();
    let absent = "absent-shard-no-such-stream";
    for (case, shard) in [
        ("the first shard id", shard_id(0)),
        ("a shard id far into the map", shard_id(42)),
    ] {
        let answer = emulator
            .call(
                "GetShardIterator",
                &request("GetShardIterator", absent, "", &shard),
            )
            .await;
        assert_refused(case, &answer, absent, &shard);
    }
}
