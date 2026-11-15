// SPDX-License-Identifier: CC0-1.0

//! What the strategy an UPDATE stored decides about a LATER write, on every write path a
//! producer has, end to end over the wire: `CreateStream` ->
//! `UpdateStreamRecordDistributionStrategy` -> `DescribeStreamSummary` -> `PutRecord` /
//! `PutRecords` -> `GetShardIterator` -> `GetRecords`.
//!
//! RULE: the record distribution strategy STORED on a stream governs every write to it,
//! whichever operation carries that write — "New records that arrive after the change are
//! distributed according to the new strategy"
//! (`UpdateStreamRecordDistributionStrategy`). Under `USER_PARTITION_KEY` the producer's
//! keys place the record: "a partition key is required", and an `ExplicitHashKey` is "the
//! hash value used to explicitly determine the shard the data record is assigned to"
//! (`PutRecordInput$PartitionKey`, `PutRecordInput$ExplicitHashKey`). Under `AUTO` the
//! service places the record itself: "the partition key is optional and any value you
//! provide is ignored, along with any ExplicitHashKey you provide. In that case, Amazon
//! Kinesis Data Streams distributes the record across shards using service-managed
//! algorithms" (`PutRecordInput$PartitionKey`). So a write that names no partition key is
//! refused under the one strategy and served under the other, an `ExplicitHashKey` names the
//! shard under the one and places nothing under the other, and switching the stream back
//! moves the rule back with it — for a single write and for one batch alike.
//!
//! The rows differ along what the rule generalises over: the write operation carrying the
//! record, the strategy stored when the write lands and how many times it has been switched,
//! which keys the write carries, the position inside a batch of the entry that carries none,
//! and which shard of the map an explicit hash key names — each shard, at both published
//! bounds of its hash key range.
//!
//! Every case drives the real awsJson1_1 request path (`POST /` with `X-Amz-Target:
//! Kinesis_20131202.<Operation>` through `MultiServiceRouter::dispatch`), so the statuses,
//! bodies and modelled errors asserted here are the ones a client sees.
//!
//! The wire harness and the ARN builder are [`crate::stream_tags`]', reused rather than
//! copied. What a READ of the stream reports, and that a switch redistributes nothing already
//! stored, is [`crate::record_distribution_strategy`]'s subject, which this suite does not
//! repeat.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_kinesis_kinesis_20131202::state::{
    ON_DEMAND_INITIAL_SHARDS, RECORD_DISTRIBUTION_AUTO, RECORD_DISTRIBUTION_USER_PARTITION_KEY,
    SHARD_ITERATOR_TYPE_TRIM_HORIZON, STREAM_ACTIVE, STREAM_MODE_ON_DEMAND,
};

use crate::stream_tags::{error_code, error_message, stream_arn, Emulator, REGION};

/// The longest a case waits for a transition the clock already owes to fall due. Generous on
/// purpose: what a case waits for is the CONDITION, re-read until it holds.
const READY_DEADLINE: Duration = Duration::from_secs(60);

/// How long a case waits between two reads of a condition that does not hold yet.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How many writes a spreading case makes: twice the shards an on-demand create allocates,
/// so a strategy that places the records itself cannot leave them all on one shard.
const WRITES_PER_CASE: usize = 2 * ON_DEMAND_INITIAL_SHARDS as usize;

/// A partition key whose own hash has nothing to do with the shard an explicit hash key
/// names, so a case that supplies both can tell which of the two placed the record.
const UNUSED_PARTITION_KEY: &str = "a-partition-key-the-hash-key-overrides";

/// How a write states the rule `PutRecordInput$PartitionKey` publishes — "If the stream uses
/// the USER_PARTITION_KEY record distribution strategy (the default), a partition key is
/// required" — written out here rather than read from the crate, so a wording change in the
/// handler is a report rather than an expectation that moves with it.
const PARTITION_KEY_REQUIRED: &str = "PartitionKey is required for a stream that uses the \
                                      USER_PARTITION_KEY record distribution strategy";

/// The `Data` member as it travels on the wire: a blob is base64-encoded when it is
/// serialized.
fn data(case: &str) -> Value {
    json!(aws_smithy_types::base64::encode(case))
}

/// Re-read the stream named `name` until it reports `ACTIVE`, which is what a transition the
/// clock owes falling due looks like from outside. No fixed wait: the condition is what is
/// waited for, under a deadline a loaded host still meets.
async fn await_active(emulator: &Emulator, case: &str, name: &str) {
    let started = Instant::now();
    loop {
        let status = emulator
            .ok(case, "DescribeStreamSummary", &json!({"StreamName": name}))
            .await["StreamDescriptionSummary"]["StreamStatus"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if status == STREAM_ACTIVE {
            return;
        }
        assert!(
            started.elapsed() < READY_DEADLINE,
            "case {case}: the stream {name} never reached {STREAM_ACTIVE}, last reported \
             {status:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One shard of the stream's map: its id and the published bounds of the hash key range it
/// covers, as `ListShards` reports them.
struct Shard {
    id: String,
    starting_hash_key: String,
    ending_hash_key: String,
}

/// Create an on-demand stream named `name` whose create names no strategy — so it starts on
/// the documented default — wait out the transition the create armed, and answer its open
/// shard map.
async fn on_demand_stream(emulator: &Emulator, case: &str, name: &str) -> Vec<Shard> {
    // An on-demand stream's shard count is the service's to manage, so the create names
    // none.
    emulator
        .ok(
            case,
            "CreateStream",
            &json!({
                "StreamName": name,
                "StreamModeDetails": {"StreamMode": STREAM_MODE_ON_DEMAND},
            }),
        )
        .await;
    await_active(emulator, case, name).await;

    let listed = emulator
        .ok(case, "ListShards", &json!({"StreamName": name}))
        .await;
    let shards: Vec<Shard> = listed["Shards"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: ListShards reported no Shards, got {listed}"))
        .iter()
        .map(|shard| {
            let bound = |member: &str| {
                shard["HashKeyRange"][member]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case}: a shard reports its {member}, got {shard}")
                    })
                    .to_string()
            };
            Shard {
                id: shard["ShardId"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case}: a shard carries no ShardId"))
                    .to_string(),
                starting_hash_key: bound("StartingHashKey"),
                ending_hash_key: bound("EndingHashKey"),
            }
        })
        .collect();
    assert_eq!(
        shards.len(),
        ON_DEMAND_INITIAL_SHARDS as usize,
        "case {case}: an on-demand create allocates {ON_DEMAND_INITIAL_SHARDS} shards, got {}",
        shards.len()
    );
    shards
}

/// Store `strategy` on the stream named `name`, asserting the modelled 200 with the empty
/// `Unit` body and then reading the stream back: what the update stored is what the stream
/// reports before any write is judged by it.
async fn set_strategy(emulator: &Emulator, case: &str, name: &str, strategy: &str) {
    let (status, answered) = emulator
        .call(
            "UpdateStreamRecordDistributionStrategy",
            &json!({
                "StreamARN": stream_arn(REGION, name),
                "RecordDistributionStrategy": strategy,
            }),
        )
        .await;
    assert_eq!(
        (status, &answered),
        (200, &json!({})),
        "case {case}: storing {strategy} answers 200 and the modelled empty body, got {status} \
         with body {answered}"
    );
    let reported = emulator
        .ok(case, "DescribeStreamSummary", &json!({"StreamName": name}))
        .await["StreamDescriptionSummary"]["RecordDistributionStrategy"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        reported, strategy,
        "case {case}: a read of the stream reports the strategy the update stored, got \
         {reported:?}"
    );
}

/// How many records the stream named `name` serves across its whole shard map, read from the
/// oldest record each shard retains.
async fn stored_records(emulator: &Emulator, case: &str, name: &str, shards: &[Shard]) -> usize {
    let mut total = 0;
    for shard in shards {
        let iterator = emulator
            .ok(
                case,
                "GetShardIterator",
                &json!({
                    "StreamName": name,
                    "ShardId": shard.id,
                    "ShardIteratorType": SHARD_ITERATOR_TYPE_TRIM_HORIZON,
                }),
            )
            .await["ShardIterator"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: expected a ShardIterator"))
            .to_string();
        let answer = emulator
            .ok(case, "GetRecords", &json!({"ShardIterator": iterator}))
            .await;
        total += answer["Records"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case}: GetRecords answered no Records, got {answer}"))
            .len();
    }
    total
}

/// The shard ids a served write reports, in the order the request carried its records:
/// `PutRecord` reports one, `PutRecords` one per entry.
fn placed_shards(case: &str, operation: &str, answer: &Value) -> Vec<String> {
    if operation == "PutRecord" {
        return vec![answer["ShardId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: PutRecord answered no ShardId, got {answer}"))
            .to_string()];
    }
    assert_eq!(
        answer["FailedRecordCount"].as_i64(),
        Some(0),
        "case {case}: a batch every record of which was stored reports a failed count of 0, got \
         {answer}"
    );
    answer["Records"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: PutRecords answered no Records, got {answer}"))
        .iter()
        .map(|result| {
            result["ShardId"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case}: a stored entry names its shard, got {result}")
                })
                .to_string()
        })
        .collect()
}

/// One row of the keyless-write table: a write that names no partition key, on one of the
/// write paths a producer has.
struct KeylessWrite {
    label: &'static str,
    /// The operation the write travels on.
    operation: &'static str,
    /// The request body, for the stream named by the argument.
    body: fn(&str) -> Value,
    /// How many records the call stores when the stored strategy serves it.
    stores: usize,
    /// The sentence the refusal carries when the stored strategy requires a partition key:
    /// `PutRecords` names the position of the entry at fault, `PutRecord` has no position to
    /// name.
    refusal: fn() -> String,
}

/// The table the partition-key rule is checked over: every write path, and both positions a
/// keyless entry can occupy inside a batch.
fn keyless_writes() -> Vec<KeylessWrite> {
    vec![
        KeylessWrite {
            label: "one write carrying no PartitionKey",
            operation: "PutRecord",
            body: |name| json!({"StreamName": name, "Data": data("keyless-single")}),
            stores: 1,
            refusal: || PARTITION_KEY_REQUIRED.to_string(),
        },
        KeylessWrite {
            label: "a batch whose only entry carries no PartitionKey",
            operation: "PutRecords",
            body: |name| {
                json!({
                    "StreamName": name,
                    "Records": [{"Data": data("keyless-batch-first")}],
                })
            },
            stores: 1,
            refusal: || format!("record 1 of the batch: {PARTITION_KEY_REQUIRED}"),
        },
        KeylessWrite {
            label: "a batch whose second entry carries no PartitionKey",
            operation: "PutRecords",
            body: |name| {
                json!({
                    "StreamName": name,
                    "Records": [
                        {"Data": data("keyless-batch-second"), "PartitionKey": "a-supplied-key"},
                        {"Data": data("keyless-batch-second")},
                    ],
                })
            },
            stores: 2,
            refusal: || format!("record 2 of the batch: {PARTITION_KEY_REQUIRED}"),
        },
    ]
}

/// The strategy stored when the write lands is what decides whether a write may omit its
/// partition key — on every write path, and however many times the stream has been switched.
#[tokio::test]
async fn the_stored_strategy_decides_whether_a_write_may_omit_its_partition_key() {
    let emulator = Emulator::new();
    for (index, row) in keyless_writes().iter().enumerate() {
        let name = format!("record-distribution-keyless-{index}");
        let shards = on_demand_stream(&emulator, row.label, &name).await;

        // The strategies the stream carries in turn: the default it starts on, then each
        // switch. `None` is "nothing has been stored yet", which is the documented default
        // `USER_PARTITION_KEY`.
        let phases: [Option<&str>; 4] = [
            None,
            Some(RECORD_DISTRIBUTION_AUTO),
            Some(RECORD_DISTRIBUTION_USER_PARTITION_KEY),
            Some(RECORD_DISTRIBUTION_AUTO),
        ];
        for phase in phases {
            let stored = phase.unwrap_or(RECORD_DISTRIBUTION_USER_PARTITION_KEY);
            let case = format!("{}/{stored}", row.label);
            if let Some(strategy) = phase {
                set_strategy(&emulator, &case, &name, strategy).await;
            }

            let before = stored_records(&emulator, &case, &name, &shards).await;
            let (status, answer) = emulator.call(row.operation, &(row.body)(&name)).await;

            if stored == RECORD_DISTRIBUTION_AUTO {
                assert_eq!(
                    status, 200,
                    "case {case}: a write naming no PartitionKey is served by a stream the \
                     service distributes itself, got {status} with body {answer}"
                );
                let placed = placed_shards(&case, row.operation, &answer);
                assert_eq!(
                    placed.len(),
                    row.stores,
                    "case {case}: the call reports a shard for each record it stored, got \
                     {placed:?}"
                );
                for shard in &placed {
                    assert!(
                        shards.iter().any(|known| &known.id == shard),
                        "case {case}: the record is placed on a shard of the stream's map, got \
                         {shard}"
                    );
                }
            } else {
                assert_eq!(
                    (status, error_code(&answer).as_str()),
                    (400, "InvalidArgumentException"),
                    "case {case}: a stream placing records by the producer's key refuses a \
                     write that carries none, got {status} with body {answer}"
                );
                assert_eq!(
                    error_message(&answer),
                    (row.refusal)(),
                    "case {case}: expected the partition-key sentence this service states, got \
                     {answer}"
                );
            }

            let after = stored_records(&emulator, &case, &name, &shards).await;
            let expected = before
                + if stored == RECORD_DISTRIBUTION_AUTO {
                    row.stores
                } else {
                    0
                };
            assert_eq!(
                after, expected,
                "case {case}: expected the stream to hold {expected} records after the call, \
                 got {after}"
            );
        }
    }
}

/// Write one record carrying `hash_key` on `operation`, answering the shard it was placed
/// on. The write supplies a partition key as well, so what the assertion sees is which of
/// the two the stored strategy placed the record by.
async fn write_with_hash_key(
    emulator: &Emulator,
    case: &str,
    operation: &str,
    name: &str,
    hash_key: &str,
) -> String {
    let body = if operation == "PutRecord" {
        json!({
            "StreamName": name,
            "PartitionKey": UNUSED_PARTITION_KEY,
            "ExplicitHashKey": hash_key,
            "Data": data(case),
        })
    } else {
        json!({
            "StreamName": name,
            "Records": [{
                "PartitionKey": UNUSED_PARTITION_KEY,
                "ExplicitHashKey": hash_key,
                "Data": data(case),
            }],
        })
    };
    let answer = emulator.ok(case, operation, &body).await;
    placed_shards(case, operation, &answer)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case}: the write reports the shard it was placed on"))
}

/// An `ExplicitHashKey` names the shard while the stream places records by the producer's
/// keys, and places nothing once the service places them itself — on both write paths, and
/// at both published bounds of every shard's hash key range.
#[tokio::test]
async fn the_stored_strategy_decides_whether_an_explicit_hash_key_places_the_record() {
    let emulator = Emulator::new();
    for operation in ["PutRecord", "PutRecords"] {
        let name = format!("record-distribution-hash-key-{}", operation.to_lowercase());
        let shards = on_demand_stream(&emulator, operation, &name).await;

        // The strategies the stream carries in turn, and whether the hash key places the
        // record under each: the default it starts on, the service-managed strategy an
        // update stores, and the default an update stores back.
        let phases: [Option<&str>; 3] = [
            None,
            Some(RECORD_DISTRIBUTION_AUTO),
            Some(RECORD_DISTRIBUTION_USER_PARTITION_KEY),
        ];
        for phase in phases {
            let stored = phase.unwrap_or(RECORD_DISTRIBUTION_USER_PARTITION_KEY);
            if let Some(strategy) = phase {
                set_strategy(&emulator, operation, &name, strategy).await;
            }

            if stored == RECORD_DISTRIBUTION_AUTO {
                // Every write names the SAME hash key, the one that owns the first shard's
                // range: a stream that placed records by it would put them all on that one
                // shard, so a spread is what "any ExplicitHashKey you provide" being ignored
                // looks like from outside.
                let case = format!("{operation}/{stored}/one hash key for every write");
                let hash_key = shards[0].starting_hash_key.clone();
                let mut placed = Vec::new();
                for _ in 0..WRITES_PER_CASE {
                    let shard =
                        write_with_hash_key(&emulator, &case, operation, &name, &hash_key).await;
                    placed.push(shard);
                }
                let mut distinct = placed.clone();
                distinct.sort();
                distinct.dedup();
                assert!(
                    distinct.len() > 1,
                    "case {case}: the hash key places nothing, so the records spread over more \
                     than one shard, got {placed:?}"
                );
                continue;
            }

            // Both published bounds of each shard's own range name that shard, and nothing
            // else does.
            for shard in &shards {
                for (bound, hash_key) in [
                    ("StartingHashKey", &shard.starting_hash_key),
                    ("EndingHashKey", &shard.ending_hash_key),
                ] {
                    let case = format!("{operation}/{stored}/{}/{bound}", shard.id);
                    let placed =
                        write_with_hash_key(&emulator, &case, operation, &name, hash_key).await;
                    assert_eq!(
                        placed, shard.id,
                        "case {case}: the hash key {hash_key} names the shard whose range \
                         covers it, got {placed}"
                    );
                }
            }
        }
    }
}
