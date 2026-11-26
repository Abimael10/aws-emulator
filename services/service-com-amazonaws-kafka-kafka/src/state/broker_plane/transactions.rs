// SPDX-License-Identifier: CC0-1.0

//! The TRANSACTION COORDINATOR a cluster's brokers run: the APIs a producer with a
//! transactional id drives, and the state that decides which of its records a consumer
//! may read.
//!
//! A transactional producer does not simply append: it asks the coordinator for a
//! producer id and an epoch (`InitProducerId`), tells it which partitions the
//! transaction touches (`AddPartitionsToTxn`) and which consumer group's offsets go with
//! it (`AddOffsetsToTxn`, then `TxnOffsetCommit`), and ends the transaction by COMMITTING
//! or ABORTING it (`EndTxn`). So the coordinator keeps, per transactional id, the
//! producer that holds it, the epoch that producer is at and what the transaction in
//! flight has touched — which is what this module is.
//!
//! What ending a transaction DOES to the log is Apache Kafka's own: a CONTROL BATCH — a
//! `COMMIT` or an `ABORT` marker — is appended to every partition the transaction
//! touched, and it takes an offset of its own, so the offsets a producer is acknowledged
//! at have a gap at each marker. The records themselves are never removed: an aborted
//! record stays in the log, and what hides it from a `read_committed` consumer is the
//! marker and the aborted-transaction list a fetch reports
//! ([`super::PartitionLog::end_transaction`]) — the client drops the records of the
//! producers it is told were aborted. Until a transaction ends, its first offset is the
//! partition's LAST STABLE OFFSET, which is as far as such a consumer reads at all.
//!
//! A SECOND `InitProducerId` on one transactional id is a new producer taking it over:
//! the epoch rises, the transaction the previous producer left open is aborted, and every
//! later call that producer makes is `PRODUCER_FENCED` — which is what makes a
//! transactional id safe to reuse after a producer dies.

use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;

use ::kafka_protocol::messages::{
    add_partitions_to_txn_response::{AddPartitionsToTxnPartitionResult, AddPartitionsToTxnTopicResult},
    txn_offset_commit_response::{TxnOffsetCommitResponsePartition, TxnOffsetCommitResponseTopic},
    AddOffsetsToTxnRequest, AddOffsetsToTxnResponse, AddPartitionsToTxnRequest,
    AddPartitionsToTxnResponse, EndTxnRequest, EndTxnResponse, InitProducerIdRequest,
    InitProducerIdResponse, ProducerId, TopicName, TxnOffsetCommitRequest, TxnOffsetCommitResponse,
};
use ::kafka_protocol::protocol::StrBytes;
use ::kafka_protocol::ResponseError;

use super::{RecordProducer, StoredRecord};
use crate::state::ComAmazonawsKafkaKafkaState;

/// The two fields a CONTROL RECORD's key carries, and the version of that key: Apache
/// Kafka writes `int16 version` then `int16 type`, where the type is `0` for an `ABORT`
/// marker and `1` for a `COMMIT` one (Apache Kafka's `ControlRecordType`). A client reads
/// the key to tell one marker from the other.
const CONTROL_KEY_VERSION: i16 = 0;
/// See [`CONTROL_KEY_VERSION`].
const CONTROL_TYPE_ABORT: i16 = 0;
/// See [`CONTROL_KEY_VERSION`].
const CONTROL_TYPE_COMMIT: i16 = 1;

/// The value of that record: `int16 version` then the `int32` epoch of the coordinator
/// that wrote the marker (Apache Kafka's `EndTransactionMarker`). One coordinator serves
/// a cluster here and it is never re-elected, so the epoch it writes is its first.
const MARKER_VALUE_VERSION: i16 = 0;
/// See [`MARKER_VALUE_VERSION`].
const COORDINATOR_EPOCH: i32 = 0;

/// The `ProducerEpoch` a transactional id nobody has taken over yet is at: the first one
/// (Apache Kafka starts a newly assigned producer id at epoch 0).
const FIRST_EPOCH: i16 = 0;

/// The most transactional ids ONE CLUSTER's coordinator holds, and the most bytes one of
/// them may carry. CHOSEN as process-safety bounds, not Apache Kafka limits: a
/// transactional id is the client's own string, so a client that names a new one on every
/// call must not be able to grow this store without end.
pub const MAX_TRANSACTIONAL_IDS_PER_CLUSTER: usize = 256;
/// See [`MAX_TRANSACTIONAL_IDS_PER_CLUSTER`].
pub const MAX_TRANSACTIONAL_ID_BYTES: usize = 256;

/// The most (topic, partition) entries one transaction may enlist, and the most consumer
/// group offsets it may carry. CHOSEN for the same reason: both lists come from the
/// client's own requests.
pub const MAX_PARTITIONS_PER_TRANSACTION: usize = 1024;
/// See [`MAX_PARTITIONS_PER_TRANSACTION`].
pub const MAX_OFFSETS_PER_TRANSACTION: usize = 512;

/// One transactional id this coordinator holds: the producer that holds it, and the
/// transaction that producer has in flight.
#[derive(Debug, Default)]
pub struct ProducerTransaction {
    /// The producer id assigned to this transactional id.
    producer_id: i64,
    /// The epoch that producer is at, which rises each time another producer takes the
    /// transactional id over.
    epoch: i16,
    /// Whether a transaction is in flight: a producer that has added a partition has
    /// begun one, and ending it clears this.
    ongoing: bool,
    /// The partitions the transaction in flight has touched, which is where its markers
    /// are written when it ends.
    partitions: BTreeSet<(String, i32)>,
    /// The consumer group offsets the transaction in flight carries, by (group id, topic,
    /// partition): a transactional producer commits the positions it consumed WITH the
    /// records it produced, so they are stored only when the transaction commits and
    /// thrown away when it aborts.
    offsets: BTreeMap<(String, String, i32), i64>,
}

impl ProducerTransaction {
    /// The producer that holds this transactional id, and the epoch it is at.
    #[must_use]
    pub fn producer(&self) -> (i64, i16) {
        (self.producer_id, self.epoch)
    }

    /// What a call quoting `producer_id` at `epoch` is refused with, and `None` when the
    /// producer holding this transactional id is the one making it.
    ///
    /// A producer id this id was never assigned is `INVALID_PRODUCER_ID_MAPPING`, an
    /// epoch BELOW the one in force is a producer another has taken the id over from
    /// (`PRODUCER_FENCED`), and one above it is an epoch no coordinator assigned
    /// (`INVALID_PRODUCER_EPOCH`).
    fn refusal(&self, producer_id: i64, epoch: i16) -> Option<i16> {
        if self.producer_id != producer_id {
            return Some(ResponseError::InvalidProducerIdMapping.code());
        }
        match epoch.cmp(&self.epoch) {
            ::std::cmp::Ordering::Less => Some(ResponseError::ProducerFenced.code()),
            ::std::cmp::Ordering::Greater => Some(ResponseError::InvalidProducerEpoch.code()),
            ::std::cmp::Ordering::Equal => None,
        }
    }

    /// Hand the transactional id to a producer that is taking it over: the epoch rises,
    /// so every call the previous producer makes is fenced, and the id is answered with
    /// the epoch the new producer writes under.
    ///
    /// At the last epoch an `int16` carries, a new PRODUCER ID is assigned at the first
    /// epoch instead — the same step Apache Kafka takes when an id's epochs run out.
    fn taken_over(&mut self, next_producer_id: &mut i64) -> (i64, i16) {
        match self.epoch.checked_add(1) {
            Some(epoch) => self.epoch = epoch,
            None => {
                self.producer_id = *next_producer_id;
                *next_producer_id += 1;
                self.epoch = FIRST_EPOCH;
            }
        }
        (self.producer_id, self.epoch)
    }
}

/// `InitProducerId`: the id a producer writes under, and — for a producer that names a
/// TRANSACTIONAL ID — the epoch it holds that id at.
///
/// A producer that names no transactional id is an idempotent one: it is given an id of
/// its own from the store's counter at the first epoch, so no two producers of this
/// emulator write under the same one. A producer that names one is given the id that
/// transactional id already carries at the NEXT epoch, which fences the producer that
/// held it before and aborts the transaction that producer left open — what a client
/// recovering a transactional id depends on.
pub fn init_producer_id(
    store: &mut ComAmazonawsKafkaKafkaState,
    cluster_arn: &str,
    request: &InitProducerIdRequest,
    now: SystemTime,
) -> InitProducerIdResponse {
    let Some(transactional_id) = request
        .transactional_id
        .as_ref()
        .map(|id| id.0.to_string())
        .filter(|id| !id.is_empty())
    else {
        let producer_id = store.next_producer_id;
        store.next_producer_id += 1;
        return InitProducerIdResponse::default()
            .with_producer_id(ProducerId(producer_id))
            .with_producer_epoch(FIRST_EPOCH);
    };
    if transactional_id.len() > MAX_TRANSACTIONAL_ID_BYTES {
        return refused_init(ResponseError::InvalidRequest.code());
    }
    let key = (cluster_arn.to_string(), transactional_id);
    if !store.producer_transactions.contains_key(&key) {
        let held = store
            .producer_transactions
            .keys()
            .filter(|(cluster, _)| *cluster == cluster_arn)
            .count();
        if held >= MAX_TRANSACTIONAL_IDS_PER_CLUSTER {
            return refused_init(ResponseError::CoordinatorNotAvailable.code());
        }
        let producer_id = store.next_producer_id;
        store.next_producer_id += 1;
        store.producer_transactions.insert(
            key,
            ProducerTransaction {
                producer_id,
                epoch: FIRST_EPOCH,
                ..Default::default()
            },
        );
        return InitProducerIdResponse::default()
            .with_producer_id(ProducerId(producer_id))
            .with_producer_epoch(FIRST_EPOCH);
    }
    // The transaction the previous holder left open is ABORTED before the id changes
    // hands: its records are in the log, and a producer that is being taken over is not
    // coming back to decide them.
    let mut next_producer_id = store.next_producer_id;
    let taken = store
        .producer_transactions
        .get_mut(&key)
        .map(|held| (held.ongoing, held.taken_over(&mut next_producer_id)));
    store.next_producer_id = next_producer_id;
    let Some((ongoing, (producer_id, epoch))) = taken else {
        return refused_init(ResponseError::CoordinatorNotAvailable.code());
    };
    if ongoing {
        end_in_flight(store, cluster_arn, &key.1, false, now);
    }
    InitProducerIdResponse::default()
        .with_producer_id(ProducerId(producer_id))
        .with_producer_epoch(epoch)
}

/// `AddPartitionsToTxn`: the partitions the transaction in flight touches, which is where
/// its commit or abort marker is written.
///
/// The versions this endpoint serves carry ONE transaction per request, so the answer is
/// the per-partition list those versions report: a refusal is reported for every
/// partition the request named, because that is the only place those versions carry one.
///
/// At most `max_entries` (topic, partition) entries of one request are answered: the list
/// comes from the client, so without a ceiling one frame would size both the work and the
/// response.
///
/// A topic in `withheld` is one this connection may not write: its partitions are refused
/// `TOPIC_AUTHORIZATION_FAILED` and none of them is enlisted, because enlisting a
/// partition is the first half of writing to it.
pub fn add_partitions(
    store: &mut ComAmazonawsKafkaKafkaState,
    cluster_arn: &str,
    request: &AddPartitionsToTxnRequest,
    withheld: &BTreeSet<String>,
    max_entries: usize,
) -> AddPartitionsToTxnResponse {
    let key = (
        cluster_arn.to_string(),
        request.v3_and_below_transactional_id.0.to_string(),
    );
    let refusal = refusal_for(
        store,
        &key,
        request.v3_and_below_producer_id.0,
        request.v3_and_below_producer_epoch,
    );
    let named: Vec<(TopicName, Vec<i32>)> = bounded_entries(
        request
            .v3_and_below_topics
            .iter()
            .map(|topic| (topic.name.clone(), topic.partitions.clone())),
        max_entries,
    );
    if refusal.is_none() {
        if let Some(held) = store.producer_transactions.get_mut(&key) {
            held.ongoing = true;
            for (topic, partitions) in &named {
                if withheld.contains(&topic.0.to_string()) {
                    continue;
                }
                for partition in partitions {
                    if held.partitions.len() >= MAX_PARTITIONS_PER_TRANSACTION {
                        break;
                    }
                    held.partitions.insert((topic.0.to_string(), *partition));
                }
            }
        }
    }
    AddPartitionsToTxnResponse::default().with_results_by_topic_v3_and_below(
        named
            .into_iter()
            .map(|(topic, partitions)| {
                let answer = match withheld.contains(&topic.0.to_string()) {
                    true => ResponseError::TopicAuthorizationFailed.code(),
                    false => refusal.unwrap_or(0),
                };
                AddPartitionsToTxnTopicResult::default()
                    .with_name(topic)
                    .with_results_by_partition(
                        partitions
                            .into_iter()
                            .map(|partition| {
                                AddPartitionsToTxnPartitionResult::default()
                                    .with_partition_index(partition)
                                    .with_partition_error_code(answer)
                            })
                            .collect(),
                    )
            })
            .collect(),
    )
}

/// The `TxnOffsetCommit` answer for a request that stored nothing: every (topic,
/// partition) it named, each carrying `error_code`, so a client is told which of its
/// positions did not land rather than told nothing about them.
#[must_use]
pub fn offsets_refused(
    request: &TxnOffsetCommitRequest,
    error_code: i16,
    max_entries: usize,
) -> TxnOffsetCommitResponse {
    let mut entries = 0usize;
    let mut results = Vec::new();
    for topic in request.topics.iter() {
        let mut partitions = Vec::new();
        for partition in topic.partitions.iter() {
            if entries >= max_entries {
                break;
            }
            entries += 1;
            partitions.push(
                TxnOffsetCommitResponsePartition::default()
                    .with_partition_index(partition.partition_index)
                    .with_error_code(error_code),
            );
        }
        results.push(
            TxnOffsetCommitResponseTopic::default()
                .with_name(topic.name.clone())
                .with_partitions(partitions),
        );
        if entries >= max_entries {
            break;
        }
    }
    TxnOffsetCommitResponse::default().with_topics(results)
}

/// The (topic, partitions) entries of one request this endpoint answers: at most
/// `max_entries` of them ACROSS the topics, so a frame naming many topics cannot make
/// each of them spend a ceiling of its own.
fn bounded_entries(
    named: impl Iterator<Item = (TopicName, Vec<i32>)>,
    max_entries: usize,
) -> Vec<(TopicName, Vec<i32>)> {
    let mut left = max_entries;
    let mut bounded = Vec::new();
    for (topic, partitions) in named {
        if left == 0 {
            break;
        }
        let taken: Vec<i32> = partitions.into_iter().take(left).collect();
        left -= taken.len();
        bounded.push((topic, taken));
    }
    bounded
}

/// `AddOffsetsToTxn`: the consumer group whose committed offsets travel with this
/// transaction, which a `TxnOffsetCommit` then states the positions of.
///
/// Apache Kafka records this as the transaction touching the group's own offsets
/// partition, which is what makes the group's commit part of the transaction: the offsets
/// are stored only if the transaction commits.
pub fn add_offsets(
    store: &mut ComAmazonawsKafkaKafkaState,
    cluster_arn: &str,
    request: &AddOffsetsToTxnRequest,
) -> AddOffsetsToTxnResponse {
    let key = (cluster_arn.to_string(), request.transactional_id.0.to_string());
    let refusal = refusal_for(
        store,
        &key,
        request.producer_id.0,
        request.producer_epoch,
    );
    if refusal.is_none() {
        if let Some(held) = store.producer_transactions.get_mut(&key) {
            held.ongoing = true;
        }
    }
    AddOffsetsToTxnResponse::default().with_error_code(refusal.unwrap_or(0))
}

/// `TxnOffsetCommit`: where the transaction's consumer has read to, held with the
/// transaction rather than written to the group at once.
///
/// A transactional producer commits what it consumed and what it produced together, so
/// the positions here become the group's committed offsets when the transaction COMMITS
/// and are thrown away when it aborts — which is what makes a read-process-write loop
/// exactly-once.
///
/// At most `max_entries` (topic, partition) entries of one request are answered, for the
/// reason [`add_partitions`] bounds its own list.
pub fn commit_offsets(
    store: &mut ComAmazonawsKafkaKafkaState,
    cluster_arn: &str,
    request: &TxnOffsetCommitRequest,
    max_entries: usize,
) -> TxnOffsetCommitResponse {
    let key = (cluster_arn.to_string(), request.transactional_id.0.to_string());
    let refusal = refusal_for(
        store,
        &key,
        request.producer_id.0,
        request.producer_epoch,
    );
    let group = request.group_id.0.to_string();
    let mut results = Vec::new();
    let mut entries = 0usize;
    for topic in request.topics.iter() {
        let name = topic.name.0.to_string();
        let mut partitions = Vec::new();
        for partition in topic.partitions.iter() {
            if entries >= max_entries {
                break;
            }
            entries += 1;
            let error_code = match refusal {
                Some(code) => code,
                None => stage_offset(
                    store,
                    &key,
                    &group,
                    &name,
                    partition.partition_index,
                    partition.committed_offset,
                ),
            };
            partitions.push(
                TxnOffsetCommitResponsePartition::default()
                    .with_partition_index(partition.partition_index)
                    .with_error_code(error_code),
            );
        }
        results.push(
            TxnOffsetCommitResponseTopic::default()
                .with_name(TopicName(StrBytes::from_string(name)))
                .with_partitions(partitions),
        );
        if entries >= max_entries {
            break;
        }
    }
    TxnOffsetCommitResponse::default().with_topics(results)
}

/// `EndTxn`: commit or abort the transaction in flight.
///
/// Every partition the transaction touched is written its marker, which takes an offset
/// of its own; a commit stores the offsets the transaction carried, and an abort throws
/// them away. The transactional id keeps its producer and epoch — the next transaction of
/// the same producer runs under them.
pub fn end_transaction(
    store: &mut ComAmazonawsKafkaKafkaState,
    cluster_arn: &str,
    request: &EndTxnRequest,
    now: SystemTime,
) -> EndTxnResponse {
    let transactional_id = request.transactional_id.0.to_string();
    let key = (cluster_arn.to_string(), transactional_id.clone());
    if let Some(code) = refusal_for(store, &key, request.producer_id.0, request.producer_epoch) {
        return EndTxnResponse::default().with_error_code(code);
    }
    // A producer that has begun no transaction has none to end, which is what
    // `INVALID_TXN_STATE` says.
    if !store
        .producer_transactions
        .get(&key)
        .is_some_and(|held| held.ongoing)
    {
        return EndTxnResponse::default()
            .with_error_code(ResponseError::InvalidTxnState.code());
    }
    end_in_flight(store, cluster_arn, &transactional_id, request.committed, now);
    EndTxnResponse::default()
}

/// What a call about the transactional id `key` quoting this producer is refused with:
/// `INVALID_PRODUCER_ID_MAPPING` for an id this coordinator holds no record of, and
/// otherwise what the record itself says ([`ProducerTransaction::refusal`]).
fn refusal_for(
    store: &ComAmazonawsKafkaKafkaState,
    key: &(String, String),
    producer_id: i64,
    epoch: i16,
) -> Option<i16> {
    match store.producer_transactions.get(key) {
        None => Some(ResponseError::InvalidProducerIdMapping.code()),
        Some(held) => held.refusal(producer_id, epoch),
    }
}

/// Hold one consumer group position with the transaction, answering the protocol error
/// code the commit of that partition carries — `0` once it is held.
fn stage_offset(
    store: &mut ComAmazonawsKafkaKafkaState,
    key: &(String, String),
    group: &str,
    topic: &str,
    partition: i32,
    offset: i64,
) -> i16 {
    let Some(held) = store.producer_transactions.get_mut(key) else {
        return ResponseError::InvalidProducerIdMapping.code();
    };
    held.ongoing = true;
    let entry = (group.to_string(), topic.to_string(), partition);
    if !held.offsets.contains_key(&entry) && held.offsets.len() >= MAX_OFFSETS_PER_TRANSACTION {
        return ResponseError::OffsetMetadataTooLarge.code();
    }
    held.offsets.insert(entry, offset);
    0
}

/// End the transaction the transactional id `transactional_id` has in flight: write its
/// marker into every partition it touched, store or throw away the offsets it carried,
/// and leave the id ready for the producer's next transaction.
fn end_in_flight(
    store: &mut ComAmazonawsKafkaKafkaState,
    cluster_arn: &str,
    transactional_id: &str,
    committed: bool,
    now: SystemTime,
) {
    let key = (cluster_arn.to_string(), transactional_id.to_string());
    let Some(held) = store.producer_transactions.get_mut(&key) else {
        return;
    };
    let producer = RecordProducer {
        id: held.producer_id,
        epoch: held.epoch,
        transactional: true,
        control: true,
    };
    let partitions: Vec<(String, i32)> = held.partitions.iter().cloned().collect();
    let offsets: Vec<((String, String, i32), i64)> =
        held.offsets.iter().map(|(at, offset)| (at.clone(), *offset)).collect();
    held.partitions.clear();
    held.offsets.clear();
    held.ongoing = false;
    for (topic, partition) in partitions {
        store.data_plane_end_transaction(
            cluster_arn,
            &topic,
            partition,
            producer,
            committed,
            now,
        );
    }
    if !committed {
        return;
    }
    for ((group, topic, partition), offset) in offsets {
        store.data_plane_commit_transactional_offset(
            cluster_arn,
            &group,
            &topic,
            partition,
            offset,
        );
    }
}

/// Whether the producer that wrote a batch has been FENCED: another producer holds the
/// transactional id it writes under, at a higher epoch.
///
/// What makes a transactional id safe to reuse: once a client has taken an id over
/// ([`init_producer_id`]), nothing the producer it replaced sends is appended, so the
/// records a transaction's marker decides are the ones the producer now holding the id
/// wrote.
#[must_use]
pub fn fenced(
    store: &ComAmazonawsKafkaKafkaState,
    cluster_arn: &str,
    producer: RecordProducer,
) -> bool {
    store
        .producer_transactions
        .iter()
        .filter(|((cluster, _), _)| cluster == cluster_arn)
        .any(|(_, held)| held.producer_id == producer.id && held.epoch > producer.epoch)
}

/// The CONTROL RECORD one marker is: its key says which marker it is and its value which
/// coordinator wrote it, both as Apache Kafka lays them out, and the batch it goes in is
/// the producer's own CONTROL batch — which is what the marker IS, whatever the caller
/// knows about the batches that producer wrote before it.
#[must_use]
pub fn marker_record(producer: RecordProducer, committed: bool, now_ms: i64) -> StoredRecord {
    let mut marker_key = Vec::new();
    marker_key.extend_from_slice(&CONTROL_KEY_VERSION.to_be_bytes());
    marker_key.extend_from_slice(
        &match committed {
            true => CONTROL_TYPE_COMMIT,
            false => CONTROL_TYPE_ABORT,
        }
        .to_be_bytes(),
    );
    let mut marker_value = Vec::new();
    marker_value.extend_from_slice(&MARKER_VALUE_VERSION.to_be_bytes());
    marker_value.extend_from_slice(&COORDINATOR_EPOCH.to_be_bytes());
    StoredRecord {
        offset: 0,
        timestamp_ms: now_ms,
        log_append_time: false,
        key: Some(marker_key),
        value: Some(marker_value),
        headers: Vec::new(),
        codec: super::RecordCodec::None,
        producer: Some(RecordProducer {
            transactional: true,
            control: true,
            ..producer
        }),
    }
}

/// The `InitProducerId` answer for a request this coordinator will not serve: the error
/// it is refused with, and no producer at all.
fn refused_init(error_code: i16) -> InitProducerIdResponse {
    InitProducerIdResponse::default()
        .with_error_code(error_code)
        .with_producer_id(ProducerId(-1))
        .with_producer_epoch(-1)
}
